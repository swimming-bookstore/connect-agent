//! Local TURN for the fold. Not a production relay.
//!
//!   demo-turn --bind 127.0.0.1:3478 --user u --pass p

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::net::UdpSocket;
use turn::auth::{generate_auth_key, AuthHandler};
use turn::relay::relay_static::RelayAddressGeneratorStatic;
use turn::server::config::{ConnConfig, ServerConfig};
use turn::server::Server;
use turn::Error;
use webrtc_util::vnet::net::Net;

#[derive(Parser)]
#[command(name = "demo-turn", about = "Demo TURN for the local fold")]
struct Cli {
    #[arg(long, default_value = "0.0.0.0:3478")]
    bind: SocketAddr,
    #[arg(long, default_value = "u")]
    user: String,
    #[arg(long, default_value = "p")]
    pass: String,
    #[arg(long, default_value = "connect")]
    realm: String,
}

struct StaticAuth {
    user: String,
    key: Vec<u8>,
}

impl AuthHandler for StaticAuth {
    fn auth_handle(
        &self,
        username: &str,
        _realm: &str,
        _src: SocketAddr,
    ) -> Result<Vec<u8>, Error> {
        if username == self.user {
            Ok(self.key.clone())
        } else {
            Err(Error::ErrFakeErr)
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "demo_turn=info".into()),
        )
        .compact()
        .init();
    let cli = Cli::parse();
    let key = generate_auth_key(&cli.user, &cli.realm, &cli.pass);
    let sock = Arc::new(UdpSocket::bind(cli.bind).await.context("bind turn")?);
    tracing::info!(bind = %cli.bind, user = %cli.user, "demo turn (not production)");
    let public_ip = if cli.bind.ip().is_unspecified() || cli.bind.ip().is_loopback() {
        connect_agent::rtc::local_ip().unwrap_or(cli.bind.ip())
    } else {
        cli.bind.ip()
    };
    tracing::info!(relay = %public_ip, "relay address");
    let server = Server::new(ServerConfig {
        conn_configs: vec![ConnConfig {
            conn: sock,
            relay_addr_generator: Box::new(RelayAddressGeneratorStatic {
                relay_address: public_ip,
                address: "0.0.0.0".into(),
                net: Arc::new(Net::new(None)),
            }),
        }],
        realm: cli.realm,
        auth_handler: Arc::new(StaticAuth {
            user: cli.user,
            key,
        }),
        channel_bind_timeout: std::time::Duration::from_secs(0),
        alloc_close_notify: None,
    })
    .await?;
    tokio::signal::ctrl_c().await?;
    server.close().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_ok_and_bad() {
        let key = generate_auth_key("u", "connect", "p");
        let a = StaticAuth {
            user: "u".into(),
            key: key.clone(),
        };
        assert_eq!(
            a.auth_handle("u", "connect", "127.0.0.1:1".parse().unwrap())
                .unwrap(),
            key
        );
        assert!(a
            .auth_handle("nope", "connect", "127.0.0.1:1".parse().unwrap())
            .is_err());
    }
}
