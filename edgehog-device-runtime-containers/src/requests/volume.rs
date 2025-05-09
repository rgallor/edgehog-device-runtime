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

//! Create image request

use astarte_device_sdk::FromEvent;

use super::ReqUuid;

/// Request to pull a Docker Volume.
#[derive(Debug, Clone, FromEvent, PartialEq, Eq, PartialOrd, Ord)]
#[from_event(
    interface = "io.edgehog.devicemanager.apps.CreateVolumeRequest",
    path = "/volume",
    rename_all = "camelCase"
)]
pub struct CreateVolume {
    pub(crate) id: ReqUuid,
    pub(crate) deployment_id: ReqUuid,
    pub(crate) driver: String,
    pub(crate) options: Vec<String>,
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fmt::Display;

    use astarte_device_sdk::{AstarteType, DeviceEvent, Value};
    use itertools::Itertools;
    use uuid::Uuid;

    use super::*;

    pub fn create_volume_request_event(
        id: impl Display,
        deployment_id: impl Display,
        driver: &str,
        options: &[&str],
    ) -> DeviceEvent {
        let options = options.iter().map(|s| s.to_string()).collect_vec();

        let fields = [
            ("id".to_string(), AstarteType::String(id.to_string())),
            (
                "deploymentId".to_string(),
                AstarteType::String(deployment_id.to_string()),
            ),
            (
                "driver".to_string(),
                AstarteType::String(driver.to_string()),
            ),
            ("options".to_string(), AstarteType::StringArray(options)),
        ]
        .into_iter()
        .collect();

        DeviceEvent {
            interface: "io.edgehog.devicemanager.apps.CreateVolumeRequest".to_string(),
            path: "/volume".to_string(),
            data: Value::Object(fields),
        }
    }

    #[test]
    fn create_volume_request() {
        let id = Uuid::new_v4();
        let deployment_id = Uuid::new_v4();
        let event = create_volume_request_event(id, deployment_id, "driver", &["foo=bar", "some="]);

        let request = CreateVolume::from_event(event).unwrap();

        let expect = CreateVolume {
            id: ReqUuid(id),
            deployment_id: ReqUuid(deployment_id),
            driver: "driver".to_string(),
            options: ["foo=bar", "some="].map(str::to_string).to_vec(),
        };

        assert_eq!(request, expect);
    }
}
