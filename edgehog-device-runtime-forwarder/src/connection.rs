// This file is part of Edgehog.
//
// Copyright 2023 SECO Mind Srl
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// SPDX-License-Identifier: Apache-2.0

//! Manage a single connection.
//!
//! A connection is responsible for sending and receiving data through a WebSocket connection from
//! and to the [`ConnectionsManager`](crate::connections_manager::ConnectionsManager).

use std::ops::{Deref, DerefMut};

use async_trait::async_trait;
use displaydoc::Display;
use futures::future::try_maybe_done;
use futures::StreamExt;
use http::Request;
use thiserror::Error as ThisError;
use tokio::sync::mpsc::{channel, Receiver, Sender, UnboundedSender};
use tokio::task::{JoinError, JoinHandle};
use tracing::{debug, error, instrument, span, warn, Level};
use tungstenite::{Error as TungError, Message as TungMessage};

use crate::connections_manager::WsStream;
use crate::messages::{
    headermap_to_hashmap, Http as ProtoHttp, HttpMessage as ProtoHttpMessage, HttpRequest,
    HttpResponse, Id, ProtoError, ProtoMessage, Protocol as ProtoProtocol,
    WebSocketMessage as ProtoWebSocketMessage,
};

pub(crate) const WS_CHANNEL_SIZE: usize = 50;

/// Connection errors.
#[non_exhaustive]
#[derive(Display, ThisError, Debug)]
pub enum ConnectionError {
    /// Channel error.
    Channel, // TODO: add a &'static str description
    /// Reqwest error.
    Http(#[from] reqwest::Error),
    /// Protobuf error.
    Protobuf(#[from] ProtoError),
    /// Failed to Join a task handle.
    JoinError(#[from] JoinError),
    /// Message sent to the wrong protocol
    WrongProtocol,
    /// Error when receiving message on websocket connection, `{0}`.
    WebSocket(#[from] TungError),
    /// Trying to poll while still connecting.
    Connecting,
}

#[derive(Debug)]
enum WriteHandle {
    Http,
    Ws(Sender<ProtoWebSocketMessage>),
}

/// Handle to the task spawned to handle a [`Connection`].
#[derive(Debug)]
pub(crate) struct ConnectionHandle {
    handle: JoinHandle<()>,
    connection: WriteHandle,
}

impl ConnectionHandle {
    /// Once a WebSocket message is received, it sends a message to the respective tokio task
    /// handling that connection.
    pub(crate) async fn send(&self, msg: ProtoProtocol) -> Result<(), ConnectionError> {
        match &self.connection {
            WriteHandle::Http => Err(ConnectionError::Channel),
            WriteHandle::Ws(.., tx_con) => {
                let message = msg.into_ws().ok_or(ConnectionError::WrongProtocol)?.message;
                tx_con
                    .send(message)
                    .await
                    .map_err(|_| ConnectionError::Channel)
            }
        }
    }
}

impl Deref for ConnectionHandle {
    type Target = JoinHandle<()>;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

impl DerefMut for ConnectionHandle {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.handle
    }
}

#[async_trait]
trait TransportBuilder {
    type Connection: Transport;

    async fn build(self) -> Result<Self::Connection, ConnectionError>;
}

#[async_trait]
trait Transport {
    async fn next(&mut self, id: &Id) -> Result<Option<ProtoMessage>, ConnectionError>;
}

struct HttpBuilder {
    /// To send the request the builder must be consumed, so the option can be replaced with None
    request: reqwest::RequestBuilder,
}

impl HttpBuilder {
    fn new(request: reqwest::RequestBuilder) -> Self {
        Self { request }
    }
}
impl TransportBuilder for HttpBuilder {
    type Connection = Http;

    async fn build(self) -> Result<Self::Connection, ConnectionError> {
        Ok(Http::new(self.request))
    }
}

impl TryFrom<HttpRequest> for HttpBuilder {
    type Error = ConnectionError;

    fn try_from(value: HttpRequest) -> Result<Self, Self::Error> {
        value
            .request_builder()
            .map(HttpBuilder::new)
            .map_err(ConnectionError::from)
    }
}

pub(crate) struct Http {
    /// To send the request the builder must be consumed, so the option can be replaced with None
    request: Option<reqwest::RequestBuilder>,
}

impl Http {
    pub(crate) fn new(request: reqwest::RequestBuilder) -> Self {
        Self {
            request: Some(request),
        }
    }
}

impl Transport for Http {
    async fn next(&mut self, id: &Id) -> Result<Option<ProtoMessage>, ConnectionError> {
        let Some(request) = self.request.take() else {
            return Ok(None);
        };

        let http_res = request.send().await?;

        // create the protobuf response to send to the bridge
        let status_code = http_res.status().into();
        let headers = headermap_to_hashmap(http_res.headers())?;
        let body = http_res.bytes().await?.into();
        let proto_res = HttpResponse::new(status_code, headers, body);

        debug!("response code {}", proto_res.status());

        let proto_msg = ProtoMessage::new(ProtoProtocol::Http(ProtoHttp::new(
            id.clone(),
            ProtoHttpMessage::Response(proto_res),
        )));

        Ok(Some(proto_msg))
    }
}

struct WebSocketBuilder {
    request: Request<()>,
    rx_con: Receiver<ProtoWebSocketMessage>,
}

impl WebSocketBuilder {
    pub(crate) fn with_handle(
        http_req: HttpRequest,
    ) -> Result<(Self, WriteHandle), ConnectionError> {
        let request = http_req.upgrade()?;

        // this channel that will be used to send data from the manager to the websocket connection
        let (tx_con, rx_con) = channel::<ProtoWebSocketMessage>(WS_CHANNEL_SIZE);

        Ok((Self { request, rx_con }, WriteHandle::Ws(tx_con)))
    }
}

impl TransportBuilder for WebSocketBuilder {
    type Connection = WebSocket;

    async fn build(self) -> Result<Self::Connection, ConnectionError> {
        // establish a websocket connection
        let (ws_con, http_res) = tokio_tungstenite::connect_async(self.request).await?;

        // send a ProtoMessage with the HTTP generated response
        let proto_msg = ProtoMessage::new(ProtoProtocol::Http(ProtoHttp::new(
            id.clone(),
            ProtoHttpMessage::Response(http_res.into()),
        )));

        Ok(WebSocket {
            ws_con,
            rx_con: self.rx_con,
        })
    }
}

struct WebSocket {
    ws_con: WsStream,
    rx_con: Receiver<ProtoWebSocketMessage>,
}

impl WebSocket {}

impl Transport for WebSocket {
    async fn next(&mut self, id: &Id) -> Result<Option<ProtoMessage>, ConnectionError> {
        // TODO: select fra rx_con.recv() e stream_next()
        // match stream_next(ws_stream).await {
        //     // ws stream closed
        //     None => Ok(None),
        //     Some(Ok(tung_msg)) => Ok(Some(ProtoMessage::try_from_tung(id.clone(), tung_msg)?)),
        //     Some(Err(err)) => Err(err),
        // }

        todo!()
    }
}

async fn stream_next(stream: &mut WsStream) -> Option<Result<TungMessage, ConnectionError>> {
    match stream.next().await {
        Some(Ok(tung_msg)) => Some(Ok(tung_msg)),
        Some(Err(err)) => Some(Err(ConnectionError::WebSocket(err))),
        None => {
            warn!("next returned None, websocket stream closed");
            // return None so that the main task_loop ends.
            None
        }
    }
}

/// Struct containing a connection information useful to communicate with the [`ConnectionsManager`](crate::connections::ConnectionsManager).
#[derive(Debug)]
pub(crate) struct Connection<T> {
    id: Id,
    tx_ws: UnboundedSender<ProtoMessage>,
    state: T,
}

impl<T> Connection<T> {
    /// Initialize a new connection.
    pub(crate) fn new(id: Id, tx_ws: UnboundedSender<ProtoMessage>, state: T) -> Self {
        Self { id, tx_ws, state }
    }

    /// Spawn the task responsible for handling the connection.
    #[instrument(skip_all)]
    pub(crate) fn spawn(self, write_handle: WriteHandle) -> ConnectionHandle
    where
        T: TransportBuilder + Send,
    {
        // spawn a task responsible for notifying when new data is available
        let handle = tokio::spawn(async move {
            // the span in used to know from which request a possible error is generated
            let span = span!(Level::DEBUG, "spawn", id = %self.id);
            let _enter = span.enter();

            if let Err(err) = self.task().await {
                error!("connection task failed with error {err:?}");
            }
        });

        ConnectionHandle {
            handle,
            connection: write_handle,
        }
    }

    /// Send an HTTP request, wait for a response, build a protobuf message and send it to the
    /// [`ConnectionsManager`](crate::connections::ConnectionsManager).
    #[instrument(skip_all, fields(id = %self.id))]
    async fn task(self) -> Result<(), ConnectionError>
    where
        T: TransportBuilder,
    {
        // TODO: add id and tx_ws on build(). eventualmente invii i protomsg
        let mut connection = self.state.build().await?;

        while let Some(proto_msg) = connection.next(&self.id).await? {
            self.tx_ws
                .send(proto_msg)
                .map_err(|_| ConnectionError::Channel)?;
        }

        Ok(())
    }
}

impl Connection<HttpBuilder> {
    /// Initialize a new Http connection.
    pub(crate) fn with_http(
        id: Id,
        tx_ws: UnboundedSender<ProtoMessage>,
        http_req: HttpRequest,
    ) -> Result<ConnectionHandle, ConnectionError> {
        let http_builder = HttpBuilder::try_from(http_req)?;
        let con = Self::new(id, tx_ws, http_builder);
        Ok(con.spawn(WriteHandle::Http))
    }
}

impl Connection<WebSocketBuilder> {
    /// Initialize a new WebSocket connection.
    pub(crate) fn with_ws(
        id: Id,
        tx_ws: UnboundedSender<ProtoMessage>,
        http_req: HttpRequest,
    ) -> Result<ConnectionHandle, ConnectionError> {
        let (ws_builder, write_handle) = WebSocketBuilder::with_handle(http_req)?;
        let con = Self::new(id, tx_ws, ws_builder);
        Ok(con.spawn(write_handle))
    }
}
