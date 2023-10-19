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

//! Internal Rust representation of protobuf structures.
//!
//! The structures belonging to this module are used to serialize/deserialize to/from the protobuf
//! data representation.

use std::borrow::{Borrow, BorrowMut};
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::num::TryFromIntError;
use std::ops::{Deref, DerefMut, Not};
use std::str::FromStr;

use displaydoc::Display;
use http::{HeaderMap, HeaderName, HeaderValue};
use prost::Message as ProstMessage;
use reqwest::{Client as ReqwClient, RequestBuilder};
use thiserror::Error as ThisError;
use tungstenite::{http::Request as TungHttpRequest, Error as TungError, Message as TungMessage};

use url::ParseError;

use edgehog_device_forwarder_proto as proto;
use edgehog_device_forwarder_proto::http::Message;
use edgehog_device_forwarder_proto::{
    http::Message as ProtoHttpMessage, http::Request as ProtoHttpRequest,
    http::Response as ProtoHttpResponse, message::Protocol as ProtoProtocol,
    web_socket::Message as ProtoWsMessage, Http as ProtoHttp, WebSocket as ProtoWebSocket,
};
use tracing::error;

/// Errors occurring while handling [`protobuf`](https://protobuf.dev/overview/) messages
#[derive(Display, ThisError, Debug)]
#[non_exhaustive]
pub enum ProtoError {
    /// Failed to serialize into Protobuf.
    Encode(#[from] prost::EncodeError),
    /// Failed to deserialize from Protobuf.
    Decode(#[from] prost::DecodeError),
    /// Empty fields.
    Empty,
    /// Reqwest error.
    Reqwest(#[from] reqwest::Error),
    /// Error parsing URL.
    ParseUrl(#[from] ParseError),
    /// Wrong HTTP method field.
    InvalidHttpMethod(#[from] http::method::InvalidMethod),
    /// Http error.
    Http(#[from] http::Error),
    /// Error while parsing Headers.
    ParseHeaders(#[from] http::header::ToStrError),
    /// Invalid port number.
    InvalidPortNumber(#[from] TryFromIntError),
    /// Wrong HTTP method field, `{0}`.
    WrongHttpMethod(String),
    /// Error performing exponential backoff when trying to connect with TTYD, {0}
    WebSocketConnect(#[from] TungError),
    /// Received a wrong WebSocket frame.
    WrongWsFrame,
}

/// Requests Id.
#[derive(Default, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct Id(Vec<u8>);

impl Debug for Id {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Id({})", hex::encode(&self.0))
    }
}

impl Display for Id {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(&self.0))
    }
}

impl Deref for Id {
    type Target = Vec<u8>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Id {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Borrow<Vec<u8>> for Id {
    fn borrow(&self) -> &Vec<u8> {
        &self.0
    }
}

impl BorrowMut<Vec<u8>> for Id {
    fn borrow_mut(&mut self) -> &mut Vec<u8> {
        &mut self.0
    }
}

impl From<Vec<u8>> for Id {
    fn from(value: Vec<u8>) -> Self {
        Id::new(value)
    }
}

impl Id {
    /// New Id.
    pub(crate) fn new(id: Vec<u8>) -> Self {
        Self(id)
    }
}

/// [`protobuf`](https://protobuf.dev/overview/) message internal representation.
#[derive(Debug)]
pub struct ProtoMessage {
    pub(crate) protocol: Protocol,
}

impl ProtoMessage {
    /// Create a [`ProtoMessage`].
    pub(crate) fn new(protocol: Protocol) -> Self {
        Self { protocol }
    }

    /// Encode [`ProtoMessage`] struct into the corresponding [`protobuf`](https://protobuf.dev/overview/) version.
    pub(crate) fn encode(self) -> Result<Vec<u8>, ProtoError> {
        let protocol = ProtoProtocol::from(self.protocol);

        let msg = proto::Message {
            protocol: Some(protocol),
        };

        let mut buf = Vec::with_capacity(msg.encoded_len());
        msg.encode(&mut buf)?;

        Ok(buf)
    }

    /// Decode a [`protobuf`](https://protobuf.dev/overview/) message into a [`ProtoMessage`] struct.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        let msg = proto::Message::decode(bytes).map_err(ProtoError::from)?;
        ProtoMessage::try_from(msg)
    }
}

/// Supported protocols.
#[derive(Debug)]
pub(crate) enum Protocol {
    Http(Http),
    WebSocket(WebSocket),
}

/// Http message.
#[derive(Debug)]
pub(crate) struct Http {
    /// Unique ID.
    pub(crate) request_id: Id,
    /// Http message type.
    pub(crate) http_msg: HttpMessage,
}

/// Http protocol message types.
#[derive(Debug)]
pub(crate) enum HttpMessage {
    Request(HttpRequest),
    Response(HttpResponse),
}

/// HTTP request fields.
#[derive(Debug)]
pub(crate) struct HttpRequest {
    path: String,
    method: String,
    query_string: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
    /// Port on the device to which the request will be sent.
    port: u16,
}

/// HTTP response fields.
#[derive(Debug)]
pub(crate) struct HttpResponse {
    status_code: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// WebSocket request fields.
#[derive(Debug)]
pub(crate) struct WebSocket {
    /// WebSocket connection identifier
    pub(crate) socket_id: Id,
    /// WebSocket message
    pub(crate) message: WebSocketMessage,
}

/// [`WebSocket`] message type.
#[derive(Debug)]
pub(crate) enum WebSocketMessage {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close { code: u16, reason: Option<String> },
}

impl ProtoMessage {
    /// Convert a Tungstenite frame into a ProtoMessage
    pub(crate) fn try_from_tung(socket_id: Id, tung_msg: TungMessage) -> Result<Self, ProtoError> {
        Ok(Self::new(Protocol::WebSocket(WebSocket {
            socket_id,
            message: WebSocketMessage::try_from(tung_msg)?,
        })))
    }
}

impl Protocol {
    pub(crate) fn into_ws(self) -> Option<WebSocket> {
        match self {
            Protocol::Http(_) => None,
            Protocol::WebSocket(ws) => Some(ws),
        }
    }
}

impl Http {
    pub(crate) fn new(request_id: Id, http_msg: HttpMessage) -> Self {
        Self {
            request_id,
            http_msg,
        }
    }
}

impl HttpRequest {
    /// Create a [`RequestBuilder`] from an HTTP request message.
    pub(crate) fn request_builder(self) -> Result<RequestBuilder, ProtoError> {
        let url_str = format!(
            "http://localhost:{}/{}?{}",
            self.port, self.path, self.query_string
        );
        let url = url::Url::parse(&url_str)?;
        let headers = HeaderMap::try_from(&self.headers)?;
        let method = http::method::Method::from_str(self.method.as_str())?;

        let http_builder = ReqwClient::new()
            .request(method, url)
            .headers(headers)
            .body(self.body);

        Ok(http_builder)
    }

    /// Check if the HTTP request contains an "Upgrade" header.
    pub(crate) fn is_upgrade(&self) -> bool {
        self.headers.contains_key("Upgrade")
    }

    pub(crate) fn upgrade(self) -> Result<TungHttpRequest<()>, ProtoError> {
        let url = format!(
            "ws://localhost:{}/{}?{}",
            self.port, self.path, self.query_string
        );

        let req = match self.method.to_ascii_uppercase().as_str() {
            "GET" => TungHttpRequest::get(url),
            "DELETE" => TungHttpRequest::delete(url),
            "HEAD" => TungHttpRequest::head(url),
            "PATCH" => TungHttpRequest::patch(url),
            "POST" => TungHttpRequest::post(url),
            "PUT" => TungHttpRequest::put(url),
            wrong_method => {
                error!("wrong HTTP method received, {}", wrong_method);
                return Err(ProtoError::WrongHttpMethod(wrong_method.to_string()));
            }
        };

        // add the headers to the request
        let req = hashmap_to_headermap(self.headers)
            .into_iter()
            .fold(req, |req, (key, val)| match key {
                Some(key) => req.header(key, val),
                None => req,
            });

        // TODO: check that the body is empty. If not, return an error (or a warning stating that it will not be used in the request)

        req.body(()).map_err(ProtoError::from)
    }
}

impl HttpResponse {
    /// Create a new HTTP response.
    pub(crate) fn new(status_code: u16, headers: HashMap<String, String>, body: Vec<u8>) -> Self {
        Self {
            status_code,
            headers,
            body,
        }
    }

    /// Return the status code of the HTTP response.
    pub(crate) fn status(&self) -> u16 {
        self.status_code
    }
}

impl WebSocketMessage {
    /// Create a text frame.
    pub(crate) fn text(data: String) -> Self {
        Self::Text(data)
    }

    /// Create a binary frame.
    pub(crate) fn binary(data: Vec<u8>) -> Self {
        Self::Binary(data)
    }

    /// Create a ping frame.
    pub(crate) fn ping(data: Vec<u8>) -> Self {
        Self::Ping(data)
    }

    /// Create a pong frame.
    pub(crate) fn pong(data: Vec<u8>) -> Self {
        Self::Pong(data)
    }

    /// Create a close frame.
    pub(crate) fn close(code: u16, reason: Option<String>) -> Self {
        Self::Close { code, reason }
    }
}

/// Convert a [`HeaderMap`] containing all HTTP headers into a [`HashMap`].
pub(crate) fn headermap_to_hashmap<'a, I>(headers: I) -> Result<HashMap<String, String>, ProtoError>
where
    I: IntoIterator<Item = (&'a HeaderName, &'a HeaderValue)>,
{
    let hm = headers
        .into_iter()
        .map(|(name, val)| {
            Ok((
                name.to_string(),
                String::from_utf8_lossy(val.as_bytes()).into(),
            ))
        })
        .collect::<Result<HashMap<String, String>, ProtoError>>()?;

    Ok(hm)
}

/// Convert a [`HashMap`] containing all HTTP headers into a [`HeaderMap`].
pub(crate) fn hashmap_to_headermap<I>(headers: I) -> HeaderMap
where
    I: IntoIterator<Item = (String, String)>,
{
    headers
        .into_iter()
        .map(|(name, val)| {
            (
                HeaderName::from_str(name.as_ref()),
                HeaderValue::from_str(val.as_ref()),
            )
        })
        // We ignore the errors here. If you want to get a list of failed conversions, you can use Iterator::partition
        // to help you out here
        .filter(|(k, v)| k.is_ok() && v.is_ok())
        .map(|(k, v)| (k.unwrap(), v.unwrap()))
        .collect()
}

impl TryFrom<proto::Message> for ProtoMessage {
    type Error = ProtoError;

    fn try_from(value: proto::Message) -> Result<Self, Self::Error> {
        let proto::Message { protocol } = value;

        let protocol = protocol.ok_or(ProtoError::Empty)?;

        Ok(ProtoMessage::new(protocol.try_into()?))
    }
}

impl TryFrom<ProtoProtocol> for Protocol {
    type Error = ProtoError;

    fn try_from(value: ProtoProtocol) -> Result<Self, Self::Error> {
        let protocol = match value {
            ProtoProtocol::Http(http) => Protocol::Http(http.try_into()?),
            ProtoProtocol::Ws(ws) => Protocol::WebSocket(ws.try_into()?),
        };

        Ok(protocol)
    }
}

impl TryFrom<ProtoHttpRequest> for HttpRequest {
    type Error = ProtoError;
    fn try_from(value: ProtoHttpRequest) -> Result<Self, Self::Error> {
        let ProtoHttpRequest {
            path,
            method,
            query_string,
            headers,
            body,
            port,
        } = value;
        Ok(Self {
            path,
            method,
            query_string,
            headers,
            body,
            port: port.try_into()?,
        })
    }
}

impl TryFrom<ProtoHttpResponse> for HttpResponse {
    type Error = ProtoError;
    fn try_from(value: ProtoHttpResponse) -> Result<Self, Self::Error> {
        let ProtoHttpResponse {
            status_code,
            headers,
            body,
        } = value;

        Ok(Self {
            status_code: status_code.try_into()?,
            headers,
            body,
        })
    }
}

impl TryFrom<ProtoHttp> for Http {
    type Error = ProtoError;

    fn try_from(value: ProtoHttp) -> Result<Self, Self::Error> {
        let ProtoHttp {
            request_id,
            message,
        } = value;

        if request_id.is_empty() || message.is_none() {
            return Err(ProtoError::Empty);
        }

        let http_msg = match message.unwrap() {
            Message::Request(req) => {
                Ok::<HttpMessage, ProtoError>(HttpMessage::Request(req.try_into()?))
            }
            Message::Response(res) => Ok(HttpMessage::Response(res.try_into()?)),
        }?;

        Ok(Http {
            request_id: request_id.into(),
            http_msg,
        })
    }
}

impl TryFrom<ProtoWebSocket> for WebSocket {
    type Error = ProtoError;

    fn try_from(value: ProtoWebSocket) -> Result<Self, Self::Error> {
        let proto::WebSocket { socket_id, message } = value;

        let Some(msg) = message else {
            return Err(Self::Error::Empty);
        };

        let message = match msg {
            ProtoWsMessage::Text(data) => WebSocketMessage::text(data),
            ProtoWsMessage::Binary(data) => WebSocketMessage::binary(data),
            ProtoWsMessage::Ping(data) => WebSocketMessage::ping(data),
            ProtoWsMessage::Pong(data) => WebSocketMessage::pong(data),
            ProtoWsMessage::Close(close) => WebSocketMessage::close(
                close.code.try_into()?,
                close.reason.is_empty().not().then_some(close.reason),
            ),
        };

        Ok(Self {
            socket_id: Id::new(socket_id),
            message,
        })
    }
}

impl From<Protocol> for ProtoProtocol {
    fn from(protocol: Protocol) -> Self {
        match protocol {
            Protocol::Http(http) => {
                let proto_http = ProtoHttp::from(http);
                ProtoProtocol::Http(proto_http)
            }
            Protocol::WebSocket(ws) => {
                let proto_ws = ProtoWebSocket::from(ws);

                ProtoProtocol::Ws(proto_ws)
            }
        }
    }
}

impl From<Http> for ProtoHttp {
    fn from(value: Http) -> Self {
        let message = match value.http_msg {
            HttpMessage::Request(req) => {
                let proto_req = ProtoHttpRequest::from(req);
                ProtoHttpMessage::Request(proto_req)
            }
            HttpMessage::Response(res) => {
                let proto_res = ProtoHttpResponse::from(res);
                ProtoHttpMessage::Response(proto_res)
            }
        };

        Self {
            request_id: value.request_id.0,
            message: Some(message),
        }
    }
}

impl From<HttpRequest> for ProtoHttpRequest {
    fn from(http_req: HttpRequest) -> Self {
        Self {
            path: http_req.path,
            method: http_req.method,
            query_string: http_req.query_string,
            headers: http_req.headers,
            body: http_req.body,
            port: http_req.port.into(),
        }
    }
}

impl From<HttpResponse> for ProtoHttpResponse {
    fn from(http_res: HttpResponse) -> Self {
        Self {
            status_code: http_res.status_code.into(),
            headers: http_res.headers,
            body: http_res.body,
        }
    }
}

impl From<WebSocket> for ProtoWebSocket {
    fn from(ws: WebSocket) -> Self {
        let ws_message = match ws.message {
            WebSocketMessage::Text(data) => ProtoWsMessage::Text(data),
            WebSocketMessage::Binary(data) => ProtoWsMessage::Binary(data),
            WebSocketMessage::Ping(data) => ProtoWsMessage::Ping(data),
            WebSocketMessage::Pong(data) => ProtoWsMessage::Pong(data),
            WebSocketMessage::Close { code, reason } => {
                ProtoWsMessage::Close(proto::web_socket::Close {
                    code: code.into(),
                    reason: reason.unwrap_or("".to_string()),
                })
            }
        };

        proto::WebSocket {
            socket_id: ws.socket_id.0,
            message: Some(ws_message),
        }
    }
}

impl TryFrom<TungMessage> for WebSocketMessage {
    type Error = ProtoError;

    fn try_from(tung_msg: TungMessage) -> Result<Self, Self::Error> {
        let msg = match tung_msg {
            tungstenite::Message::Text(data) => WebSocketMessage::text(data),
            tungstenite::Message::Binary(data) => WebSocketMessage::binary(data),
            tungstenite::Message::Ping(data) => WebSocketMessage::ping(data),
            tungstenite::Message::Pong(data) => WebSocketMessage::pong(data),
            tungstenite::Message::Close(data) => {
                // instead of returning an error, here i build a default close frame in case no frame is passed
                let (code, reason) = match data {
                    Some(close_frame) => {
                        let code = close_frame.code.into();
                        let reason = Some(close_frame.reason.into_owned());
                        (code, reason)
                    }
                    None => (1000, None),
                };

                WebSocketMessage::close(code, reason)
            }
            tungstenite::Message::Frame(_) => {
                error!("this kind of message should not be sent");
                return Err(ProtoError::WrongWsFrame);
            }
        };

        Ok(msg)
    }
}
