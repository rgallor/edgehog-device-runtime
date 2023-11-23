/*
 * This file is part of Edgehog.
 *
 * Copyright 2023 SECO Mind Srl
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *   http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

//! Manage the device forwarder operation.

use std::borrow::Borrow;
use std::sync::Arc;
use std::{
    collections::{hash_map::Entry, HashMap},
    hash::Hash,
    ops::Deref,
};

use crate::data::Publisher;
use astarte_device_sdk::types::AstarteType;
use astarte_device_sdk::{Aggregation, AstarteDeviceDataEvent};
use edgehog_forwarder::astarte::{retrieve_connection_info, ConnectionInfo};
use edgehog_forwarder::connections_manager::ConnectionsManager;
use log::error;
use reqwest::Url;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

#[derive(Debug)]
struct Key(ConnectionInfo);

impl Deref for Key {
    type Target = ConnectionInfo;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Borrow<ConnectionInfo> for Key {
    fn borrow(&self) -> &ConnectionInfo {
        &self.0
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.host == other.host && self.port == other.port
    }
}

impl Eq for Key {}

impl Hash for Key {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.host.hash(state);
        self.port.hash(state);
        self.session_token.hash(state);
    }
}

struct SessionState {
    token: String,
    connected: bool,
}

/// Struct representing the state of a remote session with a device
impl SessionState {
    fn connected(token: String) -> Self {
        Self {
            token,
            connected: true,
        }
    }

    fn disconnected(token: String) -> Self {
        Self {
            token,
            connected: false,
        }
    }
}

impl From<SessionState> for AstarteType {
    fn from(value: SessionState) -> Self {
        Self::Boolean(value.connected)
    }
}

#[derive(Debug)]
struct Session {
    handle: JoinHandle<()>,
    notify_close: Arc<Notify>,
}

impl Session {
    fn new(bridge_url: Url, session_token: String, tx_state: Sender<SessionState>) -> Self {
        // notifier to terminate the session
        let notify_close = Arc::new(Notify::new());

        // spawn a new task responsible for handling the remote terminal operations
        let handle = Self::spawn_task(
            bridge_url,
            session_token,
            tx_state,
            Arc::clone(&notify_close),
        );

        Self {
            handle,
            notify_close,
        }
    }

    fn close(&self) {
        self.notify_close.notify_one()
    }

    /// Spawn the task responsible for handling a device forwarder instance.
    fn spawn_task(
        bridge_url: Url,
        session_token: String,
        tx_state: Sender<SessionState>,
        notify_close: Arc<Notify>,
    ) -> JoinHandle<()> {
        tokio::spawn(Self::handle_session(
            bridge_url,
            session_token,
            tx_state,
            notify_close,
        ))
    }

    /// Handle remote session connection, operations and disconnection.
    async fn handle_session(
        bridge_url: Url,
        session_token: String,
        tx_state: Sender<SessionState>,
        _notify_close: Arc<Notify>,
    ) {
        // TODO: handle reception of Notify event and call con_manager.disconnect().

        // use take()
        match ConnectionsManager::connect(bridge_url.clone()).await {
            Ok(mut con_manager) => {
                Self::connect(session_token.clone(), &tx_state).await;

                // handle the connections
                if let Err(err) = con_manager.handle_connections().await {
                    error!("failed to handle connections, {err}");
                }

                Self::disconnect(session_token, &tx_state).await;
            }
            Err(err) => error!("failed to connect, {err}"),
        }
    }

    // update the session state to "connected"
    async fn connect(session_token: String, tx_state: &Sender<SessionState>) {
        if let Err(err) = tx_state.send(SessionState::connected(session_token)).await {
            error!("failed to change session state to connected, {err}");
        }
    }

    // update the session state to "disconnected"
    async fn disconnect(session_token: String, tx_state: &Sender<SessionState>) {
        if let Err(err) = tx_state
            .send(SessionState::disconnected(session_token))
            .await
        {
            error!("failed to change session state to connected, {err}");
        }
    }
}

impl Deref for Session {
    type Target = JoinHandle<()>;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

/// Device forwarder.
///
/// It maintains a collection of tokio task handles, each one identified by a [`Key`] containing
/// the connection information and responsible for providing forwarder functionalities. For
/// instance, a task could open a remote terminal between the device and a certain host.
#[derive(Debug)]
pub struct Forwarder {
    tx_state: Sender<SessionState>,
    sessions: HashMap<Key, Session>,
}

const CHANNEL_STATE_SIZE: usize = 50;

impl Forwarder {
    /// Initialize the forwarder instance, spawning also a task responsible for sending a property
    /// necessary to update the device session state.
    pub fn init<P>(publisher: P) -> Self
    where
        P: Publisher + 'static,
    {
        let (tx_state, rx_state) = channel::<SessionState>(CHANNEL_STATE_SIZE);

        // the handle is not stored because it is ended when all tx_state are dropped (which is when
        // the Forwarder instance is dropped
        let _ = tokio::spawn(Self::handle_session_state_task(publisher, rx_state));

        Self {
            tx_state,
            sessions: Default::default(),
        }
    }

    async fn handle_session_state_task<P>(publisher: P, mut rx_state: Receiver<SessionState>)
    where
        P: Publisher,
    {
        while let Some(msg) = rx_state.recv().await {
            let ipath = format!("/{}/connected", msg.token);
            let idata = msg.into();

            if let Err(err) = publisher
                .send(
                    "io.edgehog.devicemanager.DeviceForwarderSessionsState",
                    &ipath,
                    idata,
                )
                .await
            {
                error!("publisher send error, {err}");
            }
        }
    }

    /// Start a device forwarder instance.
    pub fn handle(&mut self, astarte_event: AstarteDeviceDataEvent) {
        let Some(idata) = Self::retrieve_astarte_data(astarte_event) else {
            return;
        };

        // retrieve the Url that the device must use to open a WebSocket connection with a host
        let cinfo = match retrieve_connection_info(idata) {
            Ok(cinfo) => cinfo,
            // error while retrieving the connection information from the Astarte data
            Err(err) => {
                error!("{err}");
                return;
            }
        };

        // Terminate the session in case if a close RemoteTerminalRequest is received
        let session_token = cinfo.session_token.clone();
        if cinfo.close {
            let Entry::Occupied(session) = self.get_running(cinfo) else {
                error!("Nonexistent session {session_token}");
                return;
            };

            // send notify to terminate the session
            session.get().close();
            return;
        }

        let bridge_url = match Url::try_from(&cinfo) {
            Ok(url) => url,
            Err(err) => {
                error!("invalid url, {err}");
                return;
            }
        };

        // check if the remote terminal task is already running. if not, spawn a new task and add it to the collection
        let tx_state = self.tx_state.clone();
        self.get_running(cinfo)
            .or_insert_with(|| Session::new(bridge_url, session_token, tx_state));
    }

    fn retrieve_astarte_data(
        astarte_event: AstarteDeviceDataEvent,
    ) -> Option<HashMap<String, AstarteType>> {
        if astarte_event.path != "/request" {
            error!("received data from an unknown path/interface: {astarte_event:?}");
            return None;
        }

        let Aggregation::Object(idata) = astarte_event.data else {
            error!("received wrong Aggregation data type");
            return None;
        };

        Some(idata)
    }

    /// Remove terminated sessions and return the searched one.
    fn get_running(&mut self, cinfo: ConnectionInfo) -> Entry<Key, Session> {
        // remove all finished tasks
        self.sessions.retain(|_, jh| !jh.is_finished());
        self.sessions.entry(Key(cinfo))
    }
}
