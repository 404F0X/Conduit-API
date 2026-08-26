use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use conduit_testkit::new_api_gateway::{MOCK_KEYS, MOCK_PAT, MOCK_USER_ID};
use conduit_testkit::{MockGatewayConfig, MockGatewayServer};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::var("CONDUIT_MOCK_GATEWAY_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(18_080);
    let server = MockGatewayServer::start(MockGatewayConfig {
        addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
    })
    .await?;
    println!("Conduit API NEW API mock gateway: {}", server.base_url());
    println!("PAT: {MOCK_PAT} | user ID: {MOCK_USER_ID}");
    println!("Model keys: {MOCK_KEYS:?}");
    println!("Config: {}/__mock/config", server.base_url());
    tokio::signal::ctrl_c().await?;
    server.shutdown().await.map_err(Into::into)
}
