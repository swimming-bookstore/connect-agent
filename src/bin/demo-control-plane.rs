//! In-memory TLS gRPC plane for local fold. Not the product control plane.
//!
//!   demo-control-plane --bind 127.0.0.1:4433 --ca target/demo-ca.pem
//!
//! Token is the agent name and id. Boxes and clients are the same here.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use connect_agent::pb::client_msg::Msg as CMsg;
use connect_agent::pb::plane_server::{Plane, PlaneServer};
use connect_agent::pb::server_msg::Msg as SMsg;
use connect_agent::pb::{App, ClientMsg, Delta, Hello, Peer, ServerMsg, Welcome};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status, Streaming};

const MAX: usize = 256 * 1024;

#[derive(Parser)]
#[command(name = "demo-control-plane", about = "Demo TLS plane for the local fold")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:4433")]
    bind: SocketAddr,
    /// PEM written here so agent/client can pass --tls-ca
    #[arg(long, default_value = "target/demo-ca.pem")]
    ca: PathBuf,
}

enum HubMsg {
    Join {
        hello: Hello,
        tx: mpsc::Sender<Result<ServerMsg, Status>>,
        reply: oneshot::Sender<Result<String, Status>>,
    },
    Leave {
        id: String,
    },
    App {
        from: String,
        app: App,
    },
}

struct Slot {
    name: String,
    pubkey: Vec<u8>,
    tx: mpsc::Sender<Result<ServerMsg, Status>>,
}

#[derive(Clone)]
struct Hub {
    tx: mpsc::Sender<HubMsg>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "demo_control_plane=info".into()),
        )
        .compact()
        .init();
    run(Cli::parse()).await
}

async fn run(cli: Cli) -> Result<()> {
    let (cert, key) = self_signed()?;
    if let Some(dir) = cli.ca.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    std::fs::write(&cli.ca, &cert).with_context(|| format!("write {}", cli.ca.display()))?;

    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(hub(rx));
    let svc = Hub { tx };
    tracing::info!(bind = %cli.bind, ca = %cli.ca.display(), "demo control plane (not production)");
    Server::builder()
        .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(cert, key)))?
        .add_service(
            PlaneServer::new(svc)
                .max_decoding_message_size(MAX)
                .max_encoding_message_size(MAX),
        )
        .serve(cli.bind)
        .await?;
    Ok(())
}

fn self_signed() -> Result<(String, String)> {
    let mut p = rcgen::CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()])?;
    p.distinguished_name
        .push(rcgen::DnType::CommonName, "demo-control-plane");
    let key = rcgen::KeyPair::generate()?;
    let cert = p.self_signed(&key)?;
    Ok((cert.pem(), key.serialize_pem()))
}

async fn hub(mut rx: mpsc::Receiver<HubMsg>) {
    let mut live: HashMap<String, Slot> = HashMap::new();
    let mut chans: HashMap<String, (String, String)> = HashMap::new();
    while let Some(msg) = rx.recv().await {
        match msg {
            HubMsg::Join { hello, tx, reply } => {
                let id = hello.token.trim().to_string();
                if id.is_empty() || hello.r#pub.len() != 32 {
                    let _ = reply.send(Err(Status::unauthenticated("token + 32-byte pub")));
                    continue;
                }
                if live.contains_key(&id) {
                    let _ = reply.send(Err(Status::already_exists("duplicate")));
                    continue;
                }
                let peers: Vec<Peer> = live
                    .iter()
                    .map(|(pid, s)| Peer {
                        id: pid.clone(),
                        name: s.name.clone(),
                        r#pub: s.pubkey.clone(),
                    })
                    .collect();
                let me = Peer {
                    id: id.clone(),
                    name: id.clone(),
                    r#pub: hello.r#pub.clone(),
                };
                let welcome = ServerMsg {
                    msg: Some(SMsg::Welcome(Welcome {
                        agent_id: id.clone(),
                        tenant: "demo".into(),
                        name: id.clone(),
                        peers,
                    })),
                };
                if tx.send(Ok(welcome)).await.is_err() {
                    let _ = reply.send(Err(Status::cancelled("gone")));
                    continue;
                }
                let delta = ServerMsg {
                    msg: Some(SMsg::Delta(Delta {
                        upsert: vec![me],
                        remove: vec![],
                    })),
                };
                for s in live.values() {
                    let _ = s.tx.send(Ok(delta.clone())).await;
                }
                live.insert(
                    id.clone(),
                    Slot {
                        name: id.clone(),
                        pubkey: hello.r#pub,
                        tx,
                    },
                );
                tracing::info!(%id, n = live.len(), "join");
                let _ = reply.send(Ok(id));
            }
            HubMsg::Leave { id } => {
                if live.remove(&id).is_none() {
                    continue;
                }
                chans.retain(|_, (a, b)| a != &id && b != &id);
                let delta = ServerMsg {
                    msg: Some(SMsg::Delta(Delta {
                        upsert: vec![],
                        remove: vec![id.clone()],
                    })),
                };
                for s in live.values() {
                    let _ = s.tx.send(Ok(delta.clone())).await;
                }
                tracing::info!(%id, n = live.len(), "leave");
            }
            HubMsg::App { from, mut app } => {
                if app.data.len() > MAX {
                    continue;
                }
                app.src = from.clone();
                if app.channel.is_empty() {
                    if app.dst.is_empty() || app.dst == from {
                        continue;
                    }
                    let (a, b) = if from < app.dst {
                        (from.clone(), app.dst.clone())
                    } else {
                        (app.dst.clone(), from.clone())
                    };
                    let ch = format!("{a}:{b}");
                    chans.entry(ch.clone()).or_insert((a, b));
                    app.channel = ch;
                }
                let Some((a, b)) = chans.get(&app.channel) else {
                    continue;
                };
                if from != *a && from != *b {
                    continue;
                }
                let other = if from == *a { b.clone() } else { a.clone() };
                app.dst = other.clone();
                if let Some(s) = live.get(&other) {
                    let _ = s
                        .tx
                        .send(Ok(ServerMsg {
                            msg: Some(SMsg::App(app)),
                        }))
                        .await;
                }
            }
        }
    }
}

#[tonic::async_trait]
impl Plane for Hub {
    type SessionStream = ReceiverStream<Result<ServerMsg, Status>>;

    async fn session(
        &self,
        req: Request<Streaming<ClientMsg>>,
    ) -> Result<Response<Self::SessionStream>, Status> {
        let mut inbound = req.into_inner();
        let first = inbound
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("empty"))?;
        let Some(CMsg::Hello(hello)) = first.msg else {
            return Err(Status::invalid_argument("hello first"));
        };
        let (out_tx, out_rx) = mpsc::channel(32);
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(HubMsg::Join {
                hello,
                tx: out_tx,
                reply: reply_tx,
            })
            .await
            .map_err(|_| Status::unavailable("hub"))?;
        let id = reply_rx
            .await
            .map_err(|_| Status::unavailable("hub"))??;
        let hub = self.tx.clone();
        tokio::spawn(async move {
            while let Ok(Some(msg)) = inbound.message().await {
                match msg.msg {
                    Some(CMsg::App(app)) => {
                        if hub
                            .send(HubMsg::App {
                                from: id.clone(),
                                app,
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let _ = hub.send(HubMsg::Leave { id }).await;
        });
        Ok(Response::new(ReceiverStream::new(out_rx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use connect_agent::pb::client_msg::Msg as CMsg;

    fn hello(token: &str) -> Hello {
        Hello {
            token: token.into(),
            r#pub: vec![1u8; 32],
        }
    }

    async fn join(
        hub: &mpsc::Sender<HubMsg>,
        token: &str,
    ) -> (String, mpsc::Receiver<Result<ServerMsg, Status>>) {
        let (tx, rx) = mpsc::channel(8);
        let (reply_tx, reply_rx) = oneshot::channel();
        hub.send(HubMsg::Join {
            hello: hello(token),
            tx,
            reply: reply_tx,
        })
        .await
        .unwrap();
        let id = reply_rx.await.unwrap().unwrap();
        (id, rx)
    }

    #[tokio::test]
    async fn hub_join_relay_leave() {
        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(hub(rx));
        let (a, mut a_rx) = join(&tx, "alice").await;
        let welcome = a_rx.recv().await.unwrap().unwrap();
        assert!(matches!(welcome.msg, Some(SMsg::Welcome(_))));
        let (b, mut b_rx) = join(&tx, "box").await;
        let _ = b_rx.recv().await;
        let _ = a_rx.recv().await; // delta upsert box
        tx.send(HubMsg::App {
            from: a.clone(),
            app: App {
                src: String::new(),
                dst: b.clone(),
                data: b"hi".to_vec(),
                channel: String::new(),
            },
        })
        .await
        .unwrap();
        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), b_rx.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let Some(SMsg::App(app)) = msg.msg else { panic!("not app") };
        assert_eq!(app.data, b"hi");
        assert_eq!(app.src, "alice");
        assert!(!app.channel.is_empty());
        tx.send(HubMsg::Leave { id: a }).await.unwrap();
        let delta = tokio::time::timeout(std::time::Duration::from_secs(2), b_rx.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match delta.msg {
            Some(SMsg::Delta(d)) => assert_eq!(d.remove, vec!["alice".to_string()]),
            _ => panic!("not delta"),
        }
        let _ = CMsg::Hello(hello("x"));
    }

    #[tokio::test]
    async fn hub_rejects_bad_hello() {
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(hub(rx));
        let (out, _) = mpsc::channel(1);
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(HubMsg::Join {
            hello: Hello {
                token: "".into(),
                r#pub: vec![1u8; 32],
            },
            tx: out,
            reply: reply_tx,
        })
        .await
        .unwrap();
        assert!(reply_rx.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn hub_rejects_duplicate() {
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(hub(rx));
        let _ = join(&tx, "same").await;
        let (out, _) = mpsc::channel(1);
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(HubMsg::Join {
            hello: hello("same"),
            tx: out,
            reply: reply_tx,
        })
        .await
        .unwrap();
        assert!(reply_rx.await.unwrap().is_err());
    }

    #[test]
    fn self_signed_pem() {
        let (cert, key) = self_signed().unwrap();
        assert!(cert.contains("BEGIN CERTIFICATE"));
        assert!(key.contains("BEGIN PRIVATE KEY") || key.contains("BEGIN RSA PRIVATE KEY") || key.contains("BEGIN EC PRIVATE KEY"));
    }
}
