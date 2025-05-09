/*
 * This file is part of Edgehog.
 *
 * Copyright 2022 SECO Mind Srl
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

use std::borrow::Cow;

use serde::Deserialize;

use crate::data::{publish, Publisher};

const INTERFACE: &str = "io.edgehog.devicemanager.RuntimeInfo";

pub const RUNTIME_INFO: RuntimeInfo<'static> = RuntimeInfo::read();

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeInfo<'a> {
    pub name: Cow<'a, str>,
    pub url: Cow<'a, str>,
    pub version: Cow<'a, str>,
    pub environment: Cow<'a, str>,
}

impl RuntimeInfo<'static> {
    /// Get structured data for `io.edgehog.devicemanager.RuntimeInfo` interface
    pub const fn read() -> Self {
        Self {
            name: Cow::Borrowed(env!("CARGO_PKG_NAME")),
            url: Cow::Borrowed(env!("CARGO_PKG_HOMEPAGE")),
            version: Cow::Borrowed(env!("CARGO_PKG_VERSION")),
            environment: Cow::Borrowed(env!("EDGEHOG_RUSTC_VERSION")),
        }
    }

    pub async fn send<T>(self, client: &T)
    where
        T: Publisher,
    {
        let values = [
            ("/name", self.name),
            ("/url", self.url),
            ("/version", self.version),
            ("/environment", self.environment),
        ];

        for (path, data) in values {
            publish(client, INTERFACE, path, data.as_ref()).await;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use astarte_device_sdk::AstarteType;
    use mockall::{predicate, Sequence};

    use crate::data::tests::MockPubSub;

    use super::*;

    pub(crate) fn mock_runtime_info_telemtry(client: &mut MockPubSub, seq: &mut Sequence) {
        client
            .expect_send()
            .with(
                predicate::eq("io.edgehog.devicemanager.RuntimeInfo"),
                predicate::eq("/name"),
                predicate::eq(AstarteType::from(env!("CARGO_PKG_NAME"))),
            )
            .once()
            .in_sequence(seq)
            .returning(|_, _, _| Ok(()));

        client
            .expect_send()
            .with(
                predicate::eq("io.edgehog.devicemanager.RuntimeInfo"),
                predicate::eq("/url"),
                predicate::eq(AstarteType::from(env!("CARGO_PKG_HOMEPAGE"))),
            )
            .once()
            .in_sequence(seq)
            .returning(|_, _, _| Ok(()));

        client
            .expect_send()
            .with(
                predicate::eq("io.edgehog.devicemanager.RuntimeInfo"),
                predicate::eq("/version"),
                predicate::eq(AstarteType::from(env!("CARGO_PKG_VERSION"))),
            )
            .once()
            .in_sequence(seq)
            .returning(|_, _, _| Ok(()));

        client
            .expect_send()
            .with(
                predicate::eq("io.edgehog.devicemanager.RuntimeInfo"),
                predicate::eq("/environment"),
                predicate::eq(AstarteType::from(env!("EDGEHOG_RUSTC_VERSION"))),
            )
            .once()
            .in_sequence(seq)
            .returning(|_, _, _| Ok(()));
    }

    #[tokio::test]
    async fn should_send_runtime_info() {
        let mut client = MockPubSub::new();
        let mut seq = Sequence::new();

        mock_runtime_info_telemtry(&mut client, &mut seq);

        RUNTIME_INFO.send(&client).await;
    }
}
