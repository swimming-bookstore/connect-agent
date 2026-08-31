use std::io::Write;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use tokio::sync::mpsc;
use tonic::transport::{Certificate, Channel, ClientTlsConfig};

use crate::pb::client_msg::Msg as CMsg;
use crate::pb::{App, ClientMsg, Hello};
use crate::protocol::Out;

pub async fn dial(coord: &str, tls_ca: &Path, tls_domain: &str) -> Result<Channel> {
    let uri = if coord.contains("://") {
        coord.to_string()
    } else {
        format!("https://{coord}")
    };
    let ca = std::fs::read(tls_ca).with_context(|| format!("read {}", tls_ca.display()))?;
    Channel::from_shared(uri.clone())?
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(ca))
                .domain_name(tls_domain),
        )?
        .connect()
        .await
        .with_context(|| format!("dial {uri}"))
}

pub fn hello(token: String, pk: &[u8; 32]) -> ClientMsg {
    ClientMsg {
        msg: Some(CMsg::Hello(Hello {
            token,
            r#pub: pk.to_vec(),
        })),
    }
}

pub fn app(dst: &str, channel: &str, data: Vec<u8>) -> ClientMsg {
    ClientMsg {
        msg: Some(CMsg::App(App {
            src: String::new(),
            dst: dst.into(),
            data,
            channel: channel.into(),
        })),
    }
}

pub async fn send_out(
    tx: &mpsc::Sender<ClientMsg>,
    dst: &str,
    channel: &str,
    msg: &Out,
) -> Result<()> {
    tx.send(app(dst, channel, serde_json::to_vec(msg)?))
        .await
        .map_err(|_| anyhow!("plane closed"))?;
    Ok(())
}

pub fn load_or_create_key(path: &Path) -> Result<[u8; 32]> {
    if path.exists() {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        return bytes
            .try_into()
            .map_err(|_| anyhow!("{} must be 32 bytes", path.display()));
    }
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let mut raw = [0u8; 32];
    getrandom::getrandom(&mut raw).map_err(|e| anyhow!(e))?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
        .with_context(|| format!("create {}", path.display()))?
        .write_all(&raw)?;
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Kind, Out, Video};

    #[test]
    fn hello_msg() {
        let pk = [7u8; 32];
        let m = hello("tok".into(), &pk);
        match m.msg {
            Some(CMsg::Hello(h)) => {
                assert_eq!(h.token, "tok");
                assert_eq!(h.r#pub, pk.to_vec());
            }
            _ => panic!("not hello"),
        }
    }

    #[test]
    fn app_msg() {
        let m = app("dst-1", "ch-9", b"hi".to_vec());
        match m.msg {
            Some(CMsg::App(a)) => {
                assert_eq!(a.dst, "dst-1");
                assert_eq!(a.channel, "ch-9");
                assert_eq!(a.data, b"hi");
                assert!(a.src.is_empty());
            }
            _ => panic!("not app"),
        }
    }

    #[tokio::test]
    async fn send_out_serializes() {
        let (tx, mut rx) = mpsc::channel(1);
        send_out(
            &tx,
            "d",
            "c",
            &Out::Hello {
                kind: Kind::Shell,
                video: Video::None,
                width: 0,
                height: 0,
                fps: 0,
            },
        )
        .await
        .unwrap();
        let m = rx.recv().await.unwrap();
        let Some(CMsg::App(a)) = m.msg else { panic!() };
        let out: Out = serde_json::from_slice(&a.data).unwrap();
        assert!(matches!(out, Out::Hello { kind: Kind::Shell, .. }));
    }

    #[test]
    fn load_or_create_key_roundtrip() {
        let dir = std::env::temp_dir().join(format!("connect-agent-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("k.key");
        let a = load_or_create_key(&path).unwrap();
        assert_eq!(a.len(), 32);
        let b = load_or_create_key(&path).unwrap();
        assert_eq!(a, b);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_or_create_key_rejects_wrong_size() {
        let dir = std::env::temp_dir().join(format!("connect-agent-badkey-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("k.key");
        std::fs::write(&path, b"short").unwrap();
        assert!(load_or_create_key(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
