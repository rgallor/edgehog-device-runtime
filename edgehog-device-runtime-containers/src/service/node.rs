// This file is part of Edgehog.
//
// Copyright 2024 SECO Mind Srl
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

//! Node in the containers

use std::fmt::Debug;

use petgraph::stable_graph::NodeIndex;
use tracing::instrument;

use crate::{store::StateStore, Docker};

use super::{resource::NodeType, state::State, Client, Id, Result};

/// A node containing the [`State`], [`Id`] of the resource and index of the dependencies.
///
/// Its a node in the graph of container resources, with the dependencies of other nodes as edges.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub(super) id: Id,
    pub(super) idx: NodeIndex,
    state: State,
}

impl Node {
    pub(crate) fn new(id: Id, idx: NodeIndex) -> Self {
        Self {
            id,
            idx,
            state: State::default(),
        }
    }

    pub(crate) fn with_state(id: Id, idx: NodeIndex, state: State) -> Self {
        Self { id, idx, state }
    }

    #[instrument(skip_all)]
    pub(super) async fn store<D, T>(
        &mut self,
        store: &mut StateStore,
        device: &D,
        inner: T,
    ) -> Result<()>
    where
        D: Client + Sync + 'static,
        T: Into<NodeType> + Debug,
    {
        self.state.store(&self.id, store, device, inner).await
    }

    #[instrument(skip_all)]
    pub(super) async fn up<D>(&mut self, device: &D, client: &Docker) -> Result<()>
    where
        D: Debug + Client + Sync + 'static,
    {
        self.state.up(&self.id, device, client).await
    }

    pub(crate) fn id(&self) -> &Id {
        &self.id
    }

    pub(crate) fn node_type(&self) -> Option<&NodeType> {
        match &self.state {
            State::Missing => None,
            State::Stored(nt) | State::Created(nt) | State::Up(nt) => Some(nt),
        }
    }

    pub(crate) fn state(&self) -> &State {
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_missing() {
        let id = Id::new("ab081bc6-9e71-4c3a-96ed-8374df16f764");
        let idx = NodeIndex::new(42);

        let node = Node::new(id, idx);

        assert!(node.state().is_missing())
    }
}
