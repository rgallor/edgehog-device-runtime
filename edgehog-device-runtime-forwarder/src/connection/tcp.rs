// Copyright 2023 SECO Mind Srl
// SPDX-License-Identifier: Apache-2.0

//! Define the necessary structs and traits to represent an HTTP connection.

use crate::connection::websocket::{WebSocket, WebSocketBuilder};
use crate::connection::{
    Connection, ConnectionError, ConnectionHandle, Transport, TransportBuilder, WriteHandle,
    WS_CHANNEL_SIZE,
};
use crate::connections_manager::WsStream;
use crate::messages::{Id, ProtoMessage, TcpSegment as ProtoTcpSegment};
use async_trait::async_trait;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tracing::{debug, instrument, trace};

/// Builder for an [`Tcp`] connection.
#[derive(Debug)]
pub(crate) struct TcpBuilder {
    tcp_seg: ProtoTcpSegment,
    rx_tcp: Receiver<ProtoTcpSegment>,
}

impl TcpBuilder {
    pub(crate) fn with_handle(tcp_seg: ProtoTcpSegment) -> (Self, WriteHandle) {
        // this channel that will be used to send data from the manager to the websocket connection
        let (tx_tcp, rx_tcp) = channel::<ProtoTcpSegment>(WS_CHANNEL_SIZE);

        (Self { tcp_seg, rx_tcp }, WriteHandle::Tcp(tx_tcp))
    }
}

#[async_trait]
impl TransportBuilder for TcpBuilder {
    type Connection = Tcp;

    #[instrument(skip_all)]
    async fn build(
        self,
        id: &Id,
        _tx_tcp: Sender<ProtoMessage>,
    ) -> Result<Self::Connection, ConnectionError> {
        // TODO: define addr method on ProtoTcpSegment that retrieves the ip-port to open a TCP connection
        let addr = self.tcp_seg.addr()?;
        let tcp_stream = match TcpStream::connect(addr).await {
            Ok(s) => s,
            Err(err) => {
                debug!("failed to create a TCP connection at {addr}");
                // TODO: check if error must be reported to the bridge in some wait
                return Err(ConnectionError::TcpConnect(err));
            }
        };

        trace!("TCP stream for ID {id} created");

        Ok(Tcp::new(tcp_stream, self.rx_tcp))
    }
}

/// TCP transport protocol.
#[derive(Debug)]
pub(crate) struct Tcp {
    tcp_stream: TcpStream,
    rx_tcp: Receiver<ProtoTcpSegment>,
}

impl Tcp {
    fn new(tcp_stream: TcpStream, rx_tcp: Receiver<ProtoTcpSegment>) -> Self {
        Self { tcp_stream, rx_tcp }
    }
}

impl Transport for Tcp {
    async fn next(&mut self, id: &Id) -> Result<Option<ProtoMessage>, ConnectionError> {
        // TODO: like next() of WebSocket (we must handle reception and sending of data into tcp connection)
        todo!()
    }
}

impl Connection<TcpBuilder> {
    /// Initialize a new WebSocket connection.
    #[instrument(skip_all)]
    pub(crate) fn with_tcp(
        id: Id,
        tx_ws: Sender<ProtoMessage>,
        tcp_req: ProtoTcpSegment,
    ) -> Result<ConnectionHandle, ConnectionError> {
        let (tcp_builder, write_handle) = TcpBuilder::with_handle(tcp_req);
        let con = Self::new(id, tx_ws, tcp_builder);
        Ok(con.spawn(write_handle))
    }
}
