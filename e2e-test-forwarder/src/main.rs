// Copyright 2023 SECO Mind Srl
// SPDX-License-Identifier: Apache-2.0

use clap::Parser;
use tokio::task::JoinSet;
use tracing::info;

#[derive(Parser, Debug)]
struct Cli {
    /// Host address of the forwarder server
    #[arg(long, short = 'H')]
    host: String,
    /// Port of the forwarder server
    #[arg(short, long, default_value_t = 4000)]
    port: u16,
    /// Session token
    #[arg(short, long)]
    token: String
}

#[tokio::main]
async fn main() {
    use edgehog_forwarder::test_utils::con_manager;
    let Cli{host, port, token} = Cli::parse();

    tracing_subscriber::fmt::init();

    let url = format!("ws://{host}:{port}/device/websocket?session={token}");
    let mut js = JoinSet::new();

    js.spawn(con_manager(url));

    while let Some(res) = js.join_next().await {
        info!("{res:?}");
    }
}
