// Copyright 2023 SECO Mind Srl
// SPDX-License-Identifier: Apache-2.0

//! Collection of connections and respective methods.

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use tokio::sync::mpsc::Sender;
use tracing::{debug, error, info, instrument, trace};

use crate::connection::{Connection, ConnectionHandle};
use crate::connections_manager::Error;
use crate::messages::{
    Http as ProtoHttp, HttpRequest, Id, ProtoMessage, WebSocket as ProtoWebSocket,
};

/// Collection of connections between the device and the bridge.
#[derive(Debug)]
pub(crate) struct Connections {
    /// Collection mapping every Connection ID with the corresponding [`tokio task`](tokio::task) spawned to
    /// handle it.
    connections: HashMap<Id, ConnectionHandle>,
    /// Write side of the channel used by each connection to send data to the [`ConnectionsManager`].
    /// This field is only cloned and passed to every connection when created.
    tx_ws: Sender<ProtoMessage>,
}

impl Connections {
    /// Initialize the Connections' collection.
    pub(crate) fn new(tx_ws: Sender<ProtoMessage>) -> Self {
        Self {
            connections: HashMap::new(),
            tx_ws,
        }
    }

    /// Handle the reception of an HTTP proto message from the bridge
    #[instrument(skip_all)]
    pub(crate) fn handle_http(&mut self, http: ProtoHttp) -> Result<(), Error> {
        let ProtoHttp {
            request_id,
            http_msg,
        } = http;

        // the HTTP message can't be an http response
        let http_req = http_msg.into_req().ok_or_else(|| {
            error!("Http response should not be sent by the bridge");
            Error::WrongMessage(request_id.clone())
        })?;

        // before executing the HTTP request, check if it is an Upgrade request.
        if http_req.is_ws_upgrade() {
            info!("Connection upgrade");
            return self.add_ws(request_id, http_req);
        }

        let tx_ws = self.tx_ws.clone();

        self.try_add(request_id.clone(), || {
            Connection::with_http(request_id, tx_ws, http_req).map_err(Error::from)
        })
    }

    /// Create a new WebSocket [`Connection`].
    #[instrument(skip(self))]
    fn add_ws(&mut self, request_id: Id, http_req: HttpRequest) -> Result<(), Error> {
        debug_assert!(http_req.is_ws_upgrade());

        let tx_ws = self.tx_ws.clone();

        self.try_add(request_id.clone(), || {
            Connection::with_ws(request_id, tx_ws, http_req).map_err(Error::from)
        })
    }

    /// Handle the reception of a websocket proto message from the bridge
    #[instrument(skip(self, ws))]
    pub(crate) async fn handle_ws(&mut self, ws: ProtoWebSocket) -> Result<(), Error> {
        let ProtoWebSocket { socket_id, message } = ws;

        // check if there exist a websocket connection with that id
        // and send a WebSocket message toward the task handling it
        match self.connections.entry(socket_id.clone()) {
            Entry::Occupied(entry) => {
                let handle = entry.get();
                let proto_msg = ProtoMessage::WebSocket(ProtoWebSocket {
                    socket_id: socket_id.clone(),
                    message,
                });
                handle.send(proto_msg).await.map_err(Error::from)
            }
            Entry::Vacant(_entry) => {
                error!("websocket connection {socket_id} not found");
                Err(Error::ConnectionNotFound(socket_id))
            }
        }
    }

    /// Return a connection entry only if is not finished.
    #[instrument(skip(self, f))]
    pub(crate) fn try_add<F>(&mut self, id: Id, f: F) -> Result<(), Error>
    where
        F: FnOnce() -> Result<ConnectionHandle, Error>,
    {
        // remove from the collection all the terminated connections
        trace!("removing terminated connections");
        self.remove_terminated();
        trace!("terminated connections removed");

        // check if there exist a connection with that id
        match self.connections.entry(id.clone()) {
            Entry::Occupied(mut entry) => {
                error!("entry already occupied");

                let handle = entry.get_mut();

                // check if the the connection is finished or not. If it is finished, return the
                // entry so that a new connection with the same Key can be created
                if !handle.is_finished() {
                    return Err(Error::IdAlreadyUsed(id));
                }

                debug!("connection terminated, replacing with a new connection");
                *handle = f()?;
                trace!("connection {id} replaced");
            }
            Entry::Vacant(entry) => {
                entry.insert(f()?);
                trace!("connection {id} inserted");
            }
        }

        Ok(())
    }

    /// Remove all terminated connection from the connections' collection.
    #[instrument(skip_all)]
    pub(crate) fn remove_terminated(&mut self) {
        self.connections
            .retain(|_k, con_handle| !con_handle.is_finished());
    }

    /// Terminate all connections, both active and non.
    pub(crate) fn disconnect(&mut self) {
        self.connections.values_mut().for_each(|con| con.abort());
        self.connections.clear();
    }
}
