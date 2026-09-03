//! Box dials the plane. App is control. Video is JPEG on App or WebRTC. No TUN.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

use connect_agent::chrome::Chromium;
use connect_agent::code;
use connect_agent::jpeg;
use connect_agent::plane::load_or_create_key;
use connect_agent::pb::plane_client::PlaneClient;
use connect_agent::pb::server_msg::Msg as SMsg;
use connect_agent::pb::{App, ClientMsg};
use connect_agent::plane;
use connect_agent::protocol::{In, Kind, Out, Video};
use connect_agent::rtc::{Ice, Rtc};
use connect_agent::shell::Shell;
use connect_agent::video;

#[derive(Parser)]
#[command(name = "connect-agent", about = "Chromium or shell on a private box")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:4433")]
    coord: String,
    #[arg(long)]
    token: String,
    #[arg(long, default_value = "agent.key")]
    key: std::path::PathBuf,
    #[arg(long)]
    tls_ca: std::path::PathBuf,
    #[arg(long, default_value = "localhost")]
    tls_domain: String,
    #[arg(long, default_value_t = 4)]
    max_sessions: usize,
    #[arg(long, default_value_t = 180)]
    idle_secs: u64,
    #[arg(long, default_value = "stun:stun.l.google.com:19302")]
    stun: Vec<String>,
    #[arg(long)]
    turn: Option<String>,
    #[arg(long)]
    turn_user: Option<String>,
    #[arg(long)]
    turn_pass: Option<String>,
}

struct Slot {
    peer: String,
    gen: u64,
    tx: mpsc::Sender<In>,
    gone: Option<oneshot::Receiver<()>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "connect_agent=info".into()),
        )
        .compact()
        .init();
    connect_agent::chrome::Chromium::reap_stale();
    run(Cli::parse()).await
}

async fn run(cli: Cli) -> Result<()> {
    let pk = load_or_create_key(&cli.key)?;
    let channel = plane::dial(&cli.coord, &cli.tls_ca, &cli.tls_domain).await?;
    let mut client = PlaneClient::new(channel);
    let (tx, rx) = mpsc::channel(64);
    tx.send(plane::hello(cli.token, &pk)).await?;
    let mut inbound = client.session(ReceiverStream::new(rx)).await?.into_inner();

    let welcome = loop {
        let msg = inbound.message().await?.ok_or_else(|| anyhow::anyhow!("plane closed"))?;
        if let Some(SMsg::Welcome(w)) = msg.msg {
            break w;
        }
    };
    tracing::info!(tenant = %welcome.tenant, name = %welcome.name, id = %welcome.agent_id, "welcome");

    let ice = Ice::new(
        &cli.stun,
        cli.turn.as_deref(),
        cli.turn_user.as_deref(),
        cli.turn_pass.as_deref(),
    );
    let max = cli.max_sessions.max(1);
    let idle = Duration::from_secs(cli.idle_secs);
    let mut sessions: HashMap<String, Slot> = HashMap::new();
    let mut next_gen: u64 = 1;
    let (done_tx, mut done_rx) = mpsc::channel::<(String, u64)>(32);

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            ended = done_rx.recv() => {
                if let Some((ch, gen)) = ended {
                    if sessions.get(&ch).is_some_and(|s| s.gen == gen) {
                        sessions.remove(&ch);
                        tracing::info!(channel = %ch, n = sessions.len(), "session gone");
                    }
                }
            }
            msg = inbound.message() => {
                let Some(msg) = msg? else { break };
                match msg.msg {
                    Some(SMsg::Delta(d)) => {
                        for p in d.upsert {
                            tracing::info!(peer = %p.name, id = %p.id, "online");
                        }
                        for id in d.remove {
                            tracing::info!(id, "offline");
                            sessions.retain(|ch, s| {
                                let hit = s.peer == id || ch.split(':').any(|p| p == id);
                                if hit { tracing::info!(channel = %ch, id, "stop"); }
                                !hit
                            });
                        }
                    }
                    Some(SMsg::App(a)) => {
                        on_app(&mut sessions, &mut next_gen, &done_tx, &tx, a, max, idle, ice.clone()).await;
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

async fn on_app(
    sessions: &mut HashMap<String, Slot>,
    next_gen: &mut u64,
    done_tx: &mpsc::Sender<(String, u64)>,
    tx: &mpsc::Sender<ClientMsg>,
    a: App,
    max: usize,
    idle: Duration,
    ice: Ice,
) {
    if a.channel.is_empty() {
        return;
    }
    let Ok(raw) = serde_json::from_slice::<In>(&a.data) else {
        return;
    };
    let ch = a.channel;
    let dst = a.src;
    if matches!(raw, In::Close) {
        if let Some(s) = sessions.get(&ch) {
            let _ = s.tx.try_send(In::Close);
            tracing::info!(channel = %ch, "close");
        }
        return;
    }
    sessions.retain(|_, s| !s.tx.is_closed());
    if matches!(raw, In::Open { .. }) {
        let prev = sessions.remove(&ch).map(|s| {
            let _ = s.tx.try_send(In::Close);
            s.gone
        });
        if sessions.len() >= max {
            tracing::warn!(channel = %ch, n = sessions.len(), max, "cap");
            let _ = plane::send_out(tx, &dst, &ch, &Out::Error { message: format!("cap ({max})") }).await;
            return;
        }
        let Some((cmd, kind, video, height)) = raw.start_session() else {
            return;
        };
        let (in_tx, in_rx) = mpsc::channel(32);
        let gen = *next_gen;
        *next_gen += 1;
        let _ = in_tx.send(cmd).await;
        let (gone_tx, gone_rx) = oneshot::channel();
        sessions.insert(ch.clone(), Slot { peer: dst.clone(), gen, tx: in_tx, gone: Some(gone_rx) });
        let done = done_tx.clone();
        let plane_tx = tx.clone();
        tokio::spawn(async move {
            if let Some(Some(prev)) = prev {
                let _ = prev.await;
            }
            if kind == Kind::Browser {
                Chromium::reap_stale();
            }
            match (kind, video) {
                (Kind::Shell, _) => shell_session(ch.clone(), dst, plane_tx, in_rx, idle).await,
                (Kind::Agent, _) => code::session(ch.clone(), dst, plane_tx, in_rx, idle).await,
                (Kind::Browser, Video::Webrtc) => {
                    let (w, h) = video::size(height);
                    webrtc_session(ch.clone(), dst, plane_tx, in_rx, idle, ice, w, h).await
                }
                (Kind::Browser, _) => {
                    jpeg::session(ch.clone(), dst, plane_tx, in_rx, idle, height).await
                }
            }
            let _ = gone_tx.send(());
            let _ = done.send((ch, gen)).await;
        });
        return;
    }
    if let Some(s) = sessions.get(&ch) {
        let _ = s.tx.send(raw).await;
    }
}

async fn webrtc_session(
    channel: String,
    dst: String,
    tx: mpsc::Sender<ClientMsg>,
    mut cmds: mpsc::Receiver<In>,
    idle: Duration,
    ice: Ice,
    w: u32,
    h: u32,
) {
    tracing::info!(%channel, width = w, height = h, "webrtc");
    // Previous Chromium is already dead (Open waits for it). Spawn this one, then ICE.
    let chrome = match Chromium::spawn_unless_close(w, h, &mut cmds).await {
        Ok(Some(c)) => c,
        Ok(None) => return,
        Err(e) => {
            tracing::error!("chrome: {e:#}");
            let _ = plane::send_out(&tx, &dst, &channel, &Out::Error { message: e.to_string() }).await;
            return;
        }
    };
    chrome.push_tabs(tx.clone(), channel.clone(), dst.clone());
    if plane::send_out(
        &tx,
        &dst,
        &channel,
        &Out::Hello {
            kind: Kind::Browser,
            video: Video::Webrtc,
            width: w,
            height: h,
            fps: video::FPS,
        },
    )
    .await
    .is_err()
    {
        return;
    }
    let mut pending_ice = Vec::new();
    let mut deadline = tokio::time::Instant::now() + idle;
    let rtc = loop {
        tokio::select! {
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { return };
                if matches!(cmd, In::Close) { return; }
                if !idle.is_zero() { deadline = tokio::time::Instant::now() + idle; }
                match cmd {
                    In::Offer { sdp } => {
                        match answer_until_close(ice.clone(), w, h, sdp, &mut cmds, &mut pending_ice).await {
                            Ok(Some(v)) => break v,
                            Ok(None) => return,
                            Err(e) => {
                                tracing::error!("webrtc answer: {e:#}");
                                let _ = plane::send_out(&tx, &dst, &channel, &Out::Error { message: format!("webrtc: {e}") }).await;
                                return;
                            }
                        }
                    }
                    In::Ice { candidate, sdp_mid, sdp_mline_index } => {
                        pending_ice.push((candidate, sdp_mid, sdp_mline_index));
                    }
                    cmd => {
                        if let Some(out) = chrome.apply(cmd).await {
                            if plane::send_out(&tx, &dst, &channel, &out).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline), if !idle.is_zero() => return,
        }
    };
    let (rtc, sdp, mut ice_rx) = rtc;
    let mut rtc = attach_rtc(rtc, &chrome);
    if plane::send_out(&tx, &dst, &channel, &Out::Answer { sdp }).await.is_err() {
        return;
    }
    for (candidate, sdp_mid, sdp_mline_index) in pending_ice.drain(..) {
        let _ = rtc.add_ice(candidate, sdp_mid, sdp_mline_index).await;
    }
    loop {
        tokio::select! {
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { break };
                if matches!(cmd, In::Close) { break; }
                if !idle.is_zero() { deadline = tokio::time::Instant::now() + idle; }
                match cmd {
                    In::Offer { sdp } => {
                        tracing::info!("renegotiate");
                        match answer_until_close(ice.clone(), w, h, sdp, &mut cmds, &mut pending_ice).await {
                            Ok(Some((new_rtc, answer, new_ice))) => {
                                ice_rx = new_ice;
                                rtc = attach_rtc(new_rtc, &chrome);
                                if plane::send_out(&tx, &dst, &channel, &Out::Answer { sdp: answer }).await.is_err() {
                                    break;
                                }
                            }
                            Ok(None) => break,
                            Err(e) => tracing::warn!("renegotiate: {e:#}"),
                        }
                    }
                    In::Ice { candidate, sdp_mid, sdp_mline_index } => {
                        let _ = rtc.add_ice(candidate, sdp_mid, sdp_mline_index).await;
                    }
                    cmd => {
                        if let Some(out) = chrome.apply(cmd).await {
                            if plane::send_out(&tx, &dst, &channel, &out).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
            msg = ice_rx.recv() => {
                let Some(msg) = msg else { continue };
                if plane::send_out(&tx, &dst, &channel, &msg).await.is_err() {
                    break;
                }
            }
            _ = tokio::time::sleep_until(deadline), if !idle.is_zero() => {
                tracing::info!(%channel, "idle");
                break;
            }
        }
    }
}

async fn answer_until_close(
    ice: Ice,
    w: u32,
    h: u32,
    sdp: String,
    cmds: &mut mpsc::Receiver<In>,
    pending_ice: &mut Vec<(String, Option<String>, Option<u16>)>,
) -> Result<Option<(Rtc, String, mpsc::Receiver<Out>)>> {
    let work = Rtc::answer(ice, w, h, &sdp);
    tokio::pin!(work);
    loop {
        tokio::select! {
            biased;
            cmd = cmds.recv() => match cmd {
                None | Some(In::Close) => return Ok(None),
                Some(In::Ice { candidate, sdp_mid, sdp_mline_index }) => {
                    pending_ice.push((candidate, sdp_mid, sdp_mline_index));
                }
                Some(_) => {}
            },
            r = &mut work => {
                loop {
                    match cmds.try_recv() {
                        Ok(In::Close) | Err(mpsc::error::TryRecvError::Disconnected) => {
                            return Ok(None);
                        }
                        Ok(In::Ice {
                            candidate,
                            sdp_mid,
                            sdp_mline_index,
                        }) => pending_ice.push((candidate, sdp_mid, sdp_mline_index)),
                        Ok(_) => {}
                        Err(mpsc::error::TryRecvError::Empty) => break,
                    }
                }
                return Ok(Some(r?));
            }
        }
    }
}

fn attach_rtc(rtc: Rtc, chrome: &Chromium) -> std::sync::Arc<Rtc> {
    if let Some(jpeg) = chrome.last_jpeg() {
        rtc.push_jpeg(jpeg);
    }
    let rtc = std::sync::Arc::new(rtc);
    let pump = rtc.clone();
    let mut frames = chrome.subscribe();
    tokio::spawn(async move {
        loop {
            match frames.recv().await {
                Ok(jpeg) => pump.push_jpeg(jpeg),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    rtc
}

async fn shell_session(
    channel: String,
    dst: String,
    tx: mpsc::Sender<ClientMsg>,
    mut cmds: mpsc::Receiver<In>,
    idle: Duration,
) {
    tracing::info!(%channel, "shell");
    let (mut sh, mut out) = match Shell::spawn() {
        Ok(v) => v,
        Err(e) => {
            let _ = plane::send_out(&tx, &dst, &channel, &Out::Error { message: e.to_string() }).await;
            return;
        }
    };
    if plane::send_out(
        &tx,
        &dst,
        &channel,
        &Out::Hello {
            kind: Kind::Shell,
            video: Video::None,
            width: 0,
            height: 0,
            fps: 0,
        },
    )
    .await
    .is_err()
    {
        return;
    }
    let mut deadline = tokio::time::Instant::now() + idle;
    loop {
        tokio::select! {
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { break };
                if matches!(cmd, In::Close) { break; }
                if !idle.is_zero() { deadline = tokio::time::Instant::now() + idle; }
                match cmd {
                    In::Stdin { data } => {
                        if sh.write(data.as_bytes()).await.is_err() { break; }
                    }
                    In::Resize { cols, rows } => {
                        let _ = sh.resize(cols, rows);
                    }
                    _ => {}
                }
            }
            chunk = out.recv() => {
                let Some(chunk) = chunk else { break };
                let data = String::from_utf8_lossy(&chunk).into_owned();
                if plane::send_out(&tx, &dst, &channel, &Out::Stdout { data }).await.is_err() {
                    break;
                }
            }
            _ = tokio::time::sleep_until(deadline), if !idle.is_zero() => break,
        }
    }
    let _ = plane::send_out(&tx, &dst, &channel, &Out::Exit { code: 0 }).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use connect_agent::protocol::In;
    use std::time::Duration;

    fn ice() -> Ice {
        Ice::new(&["stun:stun.l.google.com:19302".into()], None, None, None)
    }

    #[tokio::test]
    async fn on_app_ignores_empty_channel() {
        let mut sessions = HashMap::new();
        let mut next = 1u64;
        let (done_tx, _done_rx) = mpsc::channel(1);
        let (tx, _rx) = mpsc::channel(1);
        on_app(
            &mut sessions,
            &mut next,
            &done_tx,
            &tx,
            App {
                src: "a".into(),
                dst: "b".into(),
                data: serde_json::to_vec(&In::Open {
                    kind: Kind::Shell,
                    video: Video::None,
                    height: 0,
                })
                .unwrap(),
                channel: String::new(),
            },
            4,
            Duration::from_secs(30),
            ice(),
        )
        .await;
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn on_app_open_shell_and_close() {
        let mut sessions = HashMap::new();
        let mut next = 1u64;
        let (done_tx, mut done_rx) = mpsc::channel(4);
        let (tx, mut rx) = mpsc::channel(16);
        on_app(
            &mut sessions,
            &mut next,
            &done_tx,
            &tx,
            App {
                src: "alice".into(),
                dst: "box".into(),
                data: serde_json::to_vec(&In::Open {
                    kind: Kind::Shell,
                    video: Video::None,
                    height: 0,
                })
                .unwrap(),
                channel: "alice:box".into(),
            },
            4,
            Duration::from_secs(30),
            ice(),
        )
        .await;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions["alice:box"].peer, "alice");
        let hello = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("hello")
            .expect("msg");
        let Some(connect_agent::pb::client_msg::Msg::App(a)) = hello.msg else {
            panic!("not app");
        };
        let out: Out = serde_json::from_slice(&a.data).unwrap();
        assert!(matches!(out, Out::Hello { kind: Kind::Shell, .. }));

        on_app(
            &mut sessions,
            &mut next,
            &done_tx,
            &tx,
            App {
                src: "alice".into(),
                dst: "box".into(),
                data: serde_json::to_vec(&In::Close).unwrap(),
                channel: "alice:box".into(),
            },
            4,
            Duration::from_secs(30),
            ice(),
        )
        .await;
        assert!(sessions.is_empty());
        let _ = tokio::time::timeout(Duration::from_secs(3), done_rx.recv()).await;
    }

    #[tokio::test]
    async fn on_app_cap() {
        let mut sessions = HashMap::new();
        let mut next = 1u64;
        let (done_tx, _done_rx) = mpsc::channel(4);
        let (tx, mut rx) = mpsc::channel(16);
        for i in 0..2 {
            on_app(
                &mut sessions,
                &mut next,
                &done_tx,
                &tx,
                App {
                    src: "alice".into(),
                    dst: "box".into(),
                    data: serde_json::to_vec(&In::Open {
                        kind: Kind::Shell,
                        video: Video::None,
                        height: 0,
                    })
                    .unwrap(),
                    channel: format!("ch-{i}"),
                },
                1,
                Duration::from_secs(30),
                ice(),
            )
            .await;
        }
        assert_eq!(sessions.len(), 1);
        let mut saw_cap = false;
        while let Ok(Some(m)) = tokio::time::timeout(Duration::from_millis(800), rx.recv()).await {
            if let Some(connect_agent::pb::client_msg::Msg::App(a)) = m.msg {
                if let Ok(Out::Error { message }) = serde_json::from_slice::<Out>(&a.data) {
                    if message.contains("cap") {
                        saw_cap = true;
                        break;
                    }
                }
            }
        }
        assert!(saw_cap);
    }
}
