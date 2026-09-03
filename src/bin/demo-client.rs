//! Laptop demo client. Shell / Browser / Agent UI. Grok login stays here; the box coding agent asks this process to complete.

use std::collections::HashMap;
use std::io::Write;

use anyhow::{anyhow, bail, Result};
use clap::Parser;
use connect_agent::pb::plane_client::PlaneClient;
use connect_agent::pb::server_msg::Msg as SMsg;
use connect_agent::pb::{App, ClientMsg, Peer};
use connect_agent::ai;
use connect_agent::plane;
use connect_agent::protocol::{In, Kind, Out, Video};
use serde_json::Value;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;

#[derive(Parser)]
#[command(name = "demo-client", about = "Laptop client: browser / shell / AI shares")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:4433")]
    coord: String,
    #[arg(long)]
    token: String,
    #[arg(long, default_value = "client.key")]
    key: std::path::PathBuf,
    #[arg(long)]
    tls_ca: std::path::PathBuf,
    #[arg(long, default_value = "localhost")]
    tls_domain: String,
    #[arg(long)]
    peer: Option<String>,
    #[arg(long, default_value = "browser")]
    kind: String,
    #[arg(long, default_value = "jpeg")]
    video: String,
    #[arg(long, default_value = "stun:stun.l.google.com:19302")]
    stun: Vec<String>,
    #[arg(long)]
    turn: Option<String>,
    #[arg(long)]
    turn_user: Option<String>,
    #[arg(long)]
    turn_pass: Option<String>,
    #[arg(long, default_value = "127.0.0.1:3056")]
    http: String,
    #[arg(long)]
    no_http: bool,
}

#[derive(Clone, Default)]
struct View {
    me: String,
    peers: Vec<(String, String)>,
    jpeg: Option<Vec<u8>>,
    url: String,
    tabs: Value,
    stdout: String,
    last: String,
    channel: String,
    dst: String,
    kind: String,
    video: String,
    offer: String,
    ice: Vec<Value>,
    ice_servers: Value,
    answer: String,
    width: u32,
    height: u32,
    /// Height asked in Open. Hello for another size is from a session we already closed.
    height_want: u32,
    /// Offer in flight; the next Answer is for this PC.
    want_answer: bool,
    log: Vec<String>,
    thread: Vec<Value>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "demo_client=info".into()),
        )
        .compact()
        .init();
    run(Cli::parse()).await
}

async fn run(cli: Cli) -> Result<()> {
    let pk = plane::load_or_create_key(&cli.key)?;
    let channel = plane::dial(&cli.coord, &cli.tls_ca, &cli.tls_domain).await?;
    let mut client = PlaneClient::new(channel);
    let (tx, rx) = mpsc::channel(64);
    tx.send(plane::hello(cli.token.clone(), &pk)).await?;
    let mut inbound = client.session(ReceiverStream::new(rx)).await?.into_inner();

    let welcome = loop {
        let msg = inbound
            .message()
            .await?
            .ok_or_else(|| anyhow!("plane closed"))?;
        if let Some(SMsg::Welcome(w)) = msg.msg {
            break w;
        }
    };
    tracing::info!(tenant = %welcome.tenant, name = %welcome.name, id = %welcome.agent_id, "welcome");
    let mut peers: HashMap<String, String> = HashMap::new();
    for p in &welcome.peers {
        remember(&mut peers, p);
        tracing::info!(peer = %p.name, id = %p.id, "peer");
    }

    let (view_tx, _view_rx) = watch::channel(View {
        me: welcome.name.clone(),
        peers: peer_list(&peers),
        tabs: serde_json::json!([]),
        ice_servers: ice_servers(&cli.stun, cli.turn.as_deref(), cli.turn_user.as_deref(), cli.turn_pass.as_deref()),
        ..View::default()
    });

    if !cli.no_http {
        let v = view_tx.clone();
        let tx_http = tx.clone();
        let bind = cli.http.clone();
        tokio::spawn(async move {
            if let Err(e) = http(bind, v, tx_http).await {
                tracing::warn!("http: {e:#}");
            }
        });
        eprintln!("demo-client  http://{}", cli.http);
        eprintln!("  Shell · Browser · Agent  (Grok login on this laptop)");
    }

    if let Some(name) = &cli.peer {
        let dst = resolve(&peers, name)?;
        patch(&view_tx, |g| g.dst = dst);
    }

    let mut stdin = Some(BufReader::new(tokio::io::stdin()).lines());
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            line = async {
                match stdin.as_mut() {
                    Some(s) => s.next_line().await,
                    None => std::future::pending().await,
                }
            } => {
                match line {
                    Ok(Some(line)) => {
                        if let Err(e) = on_line(&tx, &mut peers, &view_tx, line.trim()).await {
                            eprintln!("{e:#}");
                        }
                    }
                    _ => stdin = None,
                }
            }
            msg = inbound.message() => {
                let Some(msg) = msg? else { break };
                match msg.msg {
                    Some(SMsg::Delta(d)) => {
                        for p in d.upsert {
                            remember(&mut peers, &p);
                            tracing::info!(peer = %p.name, id = %p.id, "online");
                        }
                        for id in d.remove {
                            let name = peers.remove(&id).unwrap_or_default();
                            tracing::info!(peer = %name, id, "offline");
                        }
                        let list = peer_list(&peers);
                        patch(&view_tx, |g| g.peers = list);
                    }
                    Some(SMsg::App(a)) => on_app(&view_tx, &tx, a),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn peer_list(peers: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut v: Vec<_> = peers.iter().map(|(id, n)| (id.clone(), n.clone())).collect();
    v.sort_by(|a, b| a.1.cmp(&b.1));
    v
}

fn patch(view: &watch::Sender<View>, f: impl FnOnce(&mut View)) {
    let mut g = view.borrow().clone();
    f(&mut g);
    let _ = view.send(g);
}

fn on_app(view: &watch::Sender<View>, tx: &mpsc::Sender<ClientMsg>, a: App) {
    let Ok(msg) = serde_json::from_slice::<Out>(&a.data) else {
        return;
    };
    let ch = a.channel.clone();
    let src = a.src.clone();
    if let Out::AiAsk { id, reset, append } = msg {
        patch(view, |g| {
            g.channel = ch.clone();
            g.dst = src.clone();
            if reset {
                g.thread = append;
            } else {
                g.thread.extend(append);
            }
        });
        let thread = view.borrow().thread.clone();
        let tx = tx.clone();
        let view = view.clone();
        tokio::spawn(async move {
            share_complete(tx, view, src, ch, id, thread).await;
        });
        return;
    }
    patch(view, |g| {
        g.channel = ch;
        g.dst = src;
        match msg {
            Out::Jpeg { data } => {
                if let Ok(j) =
                    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
                {
                    g.jpeg = Some(j);
                }
            }
            Out::Tabs { tabs, url } => {
                if !tabs.is_empty() {
                    g.url = tabs
                        .iter()
                        .find(|t| t.active)
                        .map(|t| t.url.clone())
                        .unwrap_or(url);
                    g.tabs = serde_json::to_value(tabs).unwrap_or(serde_json::json!([]));
                }
            }
            Out::Stdout { data } => {
                print!("{data}");
                let _ = std::io::stdout().flush();
                g.stdout.push_str(&data);
                if g.stdout.len() > 80_000 {
                    g.stdout = g.stdout[g.stdout.len() - 40_000..].into();
                }
            }
            Out::Hello {
                kind,
                video,
                width,
                height,
                ..
            } => {
                let kind = match kind {
                    Kind::Browser => "browser",
                    Kind::Shell => "shell",
                    Kind::Agent => "agent",
                };
                let video = match video {
                    Video::Webrtc => "webrtc",
                    Video::Jpeg => "jpeg",
                    Video::None => "none",
                };
                if kind != g.kind || video != g.video {
                    return;
                }
                if video == "webrtc" {
                    let want = if g.height_want >= 1080 { 1080 } else { 720 };
                    if height != want {
                        return;
                    }
                }
                g.width = width;
                g.height = height;
                g.offer.clear();
                g.answer.clear();
                g.ice.clear();
                g.want_answer = false;
            }
            Out::Answer { sdp } => {
                if g.want_answer {
                    g.answer = sdp;
                    g.want_answer = false;
                }
            }
            Out::Ice {
                candidate,
                sdp_mid,
                sdp_mline_index,
            } => {
                if !g.answer.is_empty() {
                    g.ice.push(serde_json::json!({
                        "type": "ice",
                        "candidate": candidate,
                        "sdp_mid": sdp_mid,
                        "sdp_mline_index": sdp_mline_index,
                    }));
                }
            }
            Out::AiChat { .. } => {
                g.thread.clear();
                g.log.clear();
                g.kind = "agent".into();
            }
            Out::Eval { result } => g.last = result,
            Out::Error { message } => {
                if g.kind == "agent" {
                    g.log.push(format!("error {message}"));
                }
            }
            Out::AiReply { text } => g.log.push(format!("ai {text}")),
            Out::AiStep { tool, result, .. } => g.log.push(format!("step {tool}: {result}")),
            Out::Exit { code } => g.last = format!("exit {code}"),
            Out::AiAsk { .. } => {}
        }
    });
}

async fn on_line(
    tx: &mpsc::Sender<ClientMsg>,
    peers: &mut HashMap<String, String>,
    view: &watch::Sender<View>,
    line: &str,
) -> Result<()> {
    if line.is_empty() {
        return Ok(());
    }
    if line == "peers" {
        for (id, n) in peers.iter() {
            println!("{n}  {id}");
        }
        return Ok(());
    }
    if let Some(rest) = line.strip_prefix("open ") {
        let mut it = rest.split_whitespace();
        let name = it.next().ok_or_else(|| anyhow!("open NAME"))?;
        let kind = parse_kind(it.next().unwrap_or("browser"))?;
        let video = parse_video(it.next().unwrap_or("jpeg"))?;
        let dst = resolve(peers, name)?;
        return open(tx, view, &dst, kind, video, 0).await;
    }
    let cmd: In = match line.split_once(' ') {
        Some(("go", url)) => In::Navigate { url: url.into() },
        Some(("eval", expression)) => In::Eval {
            expression: expression.into(),
        },
        Some(("stdin", data)) => In::Stdin {
            data: format!("{data}\n"),
        },
        Some(("tab+", url)) => In::NewTab { url: url.into() },
        Some(("tab", id)) => In::Focus { id: id.into() },
        Some(("x", id)) => In::CloseTab { id: id.into() },
        None if line == "back" => In::Back,
        None if line == "fwd" => In::Forward,
        None if line == "close" => In::Close,
        _ => bail!("unknown: {line}"),
    };
    send_in(tx, view, &cmd).await
}

async fn open(
    tx: &mpsc::Sender<ClientMsg>,
    view: &watch::Sender<View>,
    dst: &str,
    kind: Kind,
    video: Video,
    height: u32,
) -> Result<()> {
    let prev = view.borrow().channel.clone();
    if !prev.is_empty() {
        let _ = send_in(tx, view, &In::Close).await;
    }
    patch(view, |g| {
        g.dst = dst.into();
        g.channel.clear();
        g.jpeg = None;
        g.stdout.clear();
        g.kind = match kind {
            Kind::Browser => "browser",
            Kind::Shell => "shell",
            Kind::Agent => "agent",
        }
        .into();
        g.video = match video {
            Video::Webrtc => "webrtc",
            Video::Jpeg => "jpeg",
            Video::None => "none",
        }
        .into();
        g.offer.clear();
        g.answer.clear();
        g.ice.clear();
        g.last.clear();
        g.tabs = serde_json::json!([]);
        g.url.clear();
        g.width = 0;
        g.height = 0;
        g.height_want = height;
        if kind == Kind::Agent {
            g.log.clear();
            g.thread.clear();
        }
    });
    send_in(tx, view, &In::Open { kind, video, height }).await
}

async fn send_in(tx: &mpsc::Sender<ClientMsg>, view: &watch::Sender<View>, cmd: &In) -> Result<()> {
    if matches!(cmd, In::Offer { .. } | In::Open { .. }) {
        patch(view, |g| {
            g.answer.clear();
            g.ice.clear();
            g.want_answer = matches!(cmd, In::Offer { .. });
        });
    }
    let (dst, ch) = {
        let g = view.borrow();
        (g.dst.clone(), g.channel.clone())
    };
    if dst.is_empty() {
        bail!("no session — pick a box");
    }
    tx.send(plane::app(&dst, &ch, serde_json::to_vec(cmd)?))
        .await?;
    Ok(())
}

async fn http(bind: String, view: watch::Sender<View>, tx: mpsc::Sender<ClientMsg>) -> Result<()> {
    let lis = TcpListener::bind(&bind).await?;
    loop {
        let (s, _) = lis.accept().await?;
        let view = view.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let _ = serve(s, view, tx).await;
        });
    }
}

async fn serve(
    mut s: TcpStream,
    view: watch::Sender<View>,
    tx: mpsc::Sender<ClientMsg>,
) -> Result<()> {
    let mut buf = vec![0u8; 65536];
    let n = s.read(&mut buf).await?;
    let raw = &buf[..n];
    let req = String::from_utf8_lossy(raw);
    let line = req.lines().next().unwrap_or("");
    let mut it = line.split_whitespace();
    let method = it.next().unwrap_or("");
    let path = it.next().unwrap_or("/");
    let path = path.split('?').next().unwrap_or(path);
    let g = view.borrow().clone();
    let (code, ctype, body) = if method == "GET" && (path == "/" || path.starts_with("/?")) {
        (200, "text/html; charset=utf-8", INDEX.as_bytes().to_vec())
    } else if method == "GET" && path == "/pkg/demo_client_ui.js" {
        (
            200,
            "text/javascript; charset=utf-8",
            include_str!(concat!(env!("OUT_DIR"), "/webui/demo_client_ui.js")).as_bytes().to_vec(),
        )
    } else if method == "GET" && path == "/pkg/demo_client_ui_bg.wasm" {
        (
            200,
            "application/wasm",
            include_bytes!(concat!(env!("OUT_DIR"), "/webui/demo_client_ui_bg.wasm")).to_vec(),
        )
    } else if method == "GET" && path == "/xterm.css" {
        (200, "text/css; charset=utf-8", XTERM_CSS.as_bytes().to_vec())
    } else if method == "GET" && path == "/xterm.js" {
        (200, "text/javascript; charset=utf-8", XTERM_JS.as_bytes().to_vec())
    } else if method == "GET" && path == "/xterm-addon-fit.js" {
        (200, "text/javascript; charset=utf-8", XTERM_FIT.as_bytes().to_vec())
    } else if method == "GET" && path == "/shot" {
        match g.jpeg {
            Some(j) => (200, "image/jpeg", j),
            None => (204, "text/plain", Vec::new()),
        }
    } else if method == "GET" && path == "/state" {
        let peers: Vec<_> = g
            .peers
            .iter()
            .map(|(id, name)| serde_json::json!({"id": id, "name": name}))
            .collect();
        (
            200,
            "application/json",
            serde_json::to_vec(&serde_json::json!({
                "me": g.me,
                "peers": peers,
                "dst": g.dst,
                "kind": g.kind,
                "video": g.video,
                "width": g.width,
                "height": g.height,
                "answer": g.answer,
                "ice": g.ice,
                "ice_servers": g.ice_servers,
                "url": g.url,
                "tabs": if g.tabs.is_array() { g.tabs.clone() } else { serde_json::json!([]) },
                "stdout": g.stdout,
                "last": g.last,
                "log": g.log,
                "grok": connect_agent::ai::info(),
            }))?,
        )
    } else if method == "POST" && path == "/cmd" {
        let body = body_of(&req, raw);
        let v: Value = serde_json::from_str(&body).unwrap_or(serde_json::json!({}));
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "open" => {
                let dst = v.get("dst").and_then(Value::as_str).unwrap_or("");
                let kind = parse_kind(v.get("kind").and_then(Value::as_str).unwrap_or("browser"));
                let video = parse_video(v.get("video").and_then(Value::as_str).unwrap_or("jpeg"));
                if let (Ok(kind), Ok(video)) = (kind, video) {
                    if !dst.is_empty() {
                        let height = v.get("height").and_then(Value::as_u64).unwrap_or(0) as u32;
                        let _ = open(&tx, &view, dst, kind, video, height).await;
                    }
                }
            }
            "ask" => {
                let text = v.get("text").and_then(Value::as_str).unwrap_or("").trim();
                if !text.is_empty() {
                    ask(&tx, &view, text).await;
                }
            }
            "new_chat" => {
                let _ = send_in(&tx, &view, &In::AiNew).await;
            }
            "login" => {
                match ai::login_start().await {
                    Ok(_) => {}
                    Err(e) => patch(&view, |g| g.log.push(format!("error {e:#}"))),
                }
            }
            "logout" => {
                let _ = ai::logout().await;
            }
            _ => {
                if let Ok(cmd) = serde_json::from_value::<In>(v) {
                    if let Err(e) = forward(&tx, &view, &cmd).await {
                        tracing::warn!("cmd: {e:#}");
                    }
                }
            }
        }
        (200, "text/plain", b"ok".to_vec())
    } else {
        (404, "text/plain", b"no".to_vec())
    };
    let h = format!(
        "HTTP/1.1 {code} OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(h.as_bytes()).await?;
    s.write_all(&body).await?;
    Ok(())
}

async fn ask(tx: &mpsc::Sender<ClientMsg>, view: &watch::Sender<View>, text: &str) {
    patch(view, |g| g.log.push(format!("you {text}")));
    let dst = {
        let g = view.borrow();
        g.dst.clone()
    };
    if dst.is_empty() {
        patch(view, |g| g.log.push("error pick a box".into()));
        return;
    }
    if view.borrow().kind != "agent" || view.borrow().channel.is_empty() {
        let _ = open(tx, view, &dst, Kind::Agent, Video::None, 0).await;
        if let Err(e) = wait_session(view, "agent").await {
            patch(view, |g| g.log.push(format!("error {e:#}")));
            return;
        }
    }
    let _ = send_in(tx, view, &In::AiUser { text: text.into() }).await;
}

async fn forward(tx: &mpsc::Sender<ClientMsg>, view: &watch::Sender<View>, cmd: &In) -> Result<()> {
    match cmd {
        In::Stdin { .. } | In::Resize { .. } => wait_session(view, "shell").await?,
        In::Offer { .. } | In::Ice { .. } => {
            let g = view.borrow();
            if g.kind != "browser" || g.channel.is_empty() || g.width == 0 {
                return Ok(());
            }
        }
        _ => {}
    }
    send_in(tx, view, cmd).await
}

async fn wait_session(view: &watch::Sender<View>, kind: &str) -> Result<()> {
    let mut rx = view.subscribe();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        {
            let g = rx.borrow();
            if g.kind == kind && !g.channel.is_empty() {
                return Ok(());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("{kind} session did not start");
        }
        tokio::select! {
            r = rx.changed() => {
                if r.is_err() {
                    bail!("view closed");
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(40)) => {}
        }
    }
}

async fn share_complete(
    tx: mpsc::Sender<ClientMsg>,
    view: watch::Sender<View>,
    dst: String,
    ch: String,
    id: String,
    thread: Vec<Value>,
) {
    let tools = connect_agent::code::coding_tools();
    let result = match ai::complete_local(&Value::Array(thread), &tools).await {
        Ok(c) => {
            if c.tool_calls.is_empty() && !c.content.is_empty() {
                patch(&view, |g| {
                    g.thread.push(serde_json::json!({"role":"assistant","content": c.content}));
                });
            }
            In::AiResult {
                id,
                content: c.content,
                tool_calls: c.tool_calls,
                error: None,
            }
        }
        Err(e) => In::AiResult {
            id,
            content: String::new(),
            tool_calls: Vec::new(),
            error: Some(e.to_string()),
        },
    };
    let Ok(bytes) = serde_json::to_vec(&result) else {
        return;
    };
    if tx.send(plane::app(&dst, &ch, bytes)).await.is_err() {
        patch(&view, |g| g.log.push("error AI share send failed".into()));
    }
}

fn body_of(head: &str, raw: &[u8]) -> String {
    head.find("\r\n\r\n")
        .map(|i| String::from_utf8_lossy(&raw[i + 4..]).into())
        .unwrap_or_default()
}

fn remember(peers: &mut HashMap<String, String>, p: &Peer) {
    peers.insert(p.id.clone(), p.name.clone());
}

fn resolve(peers: &HashMap<String, String>, name: &str) -> Result<String> {
    if peers.contains_key(name) {
        return Ok(name.into());
    }
    peers
        .iter()
        .find(|(_, n)| *n == name)
        .map(|(id, _)| id.clone())
        .ok_or_else(|| anyhow!("no peer {name}"))
}

fn ice_servers(stun: &[String], turn: Option<&str>, user: Option<&str>, pass: Option<&str>) -> Value {
    let mut v = Vec::new();
    for u in stun {
        if !u.is_empty() {
            v.push(serde_json::json!({"urls": [u]}));
        }
    }
    if let Some(t) = turn.filter(|s| !s.is_empty()) {
        let urls = connect_agent::rtc::browser_turn_urls(t);
        v.push(serde_json::json!({
            "urls": urls,
            "username": user.unwrap_or(""),
            "credential": pass.unwrap_or(""),
        }));
    }
    if v.is_empty() {
        v.push(serde_json::json!({"urls": ["stun:stun.l.google.com:19302"]}));
    }
    Value::Array(v)
}

fn parse_kind(s: &str) -> Result<Kind> {
    match s {
        "browser" => Ok(Kind::Browser),
        "shell" => Ok(Kind::Shell),
        "agent" => Ok(Kind::Agent),
        _ => bail!("kind browser|shell|agent"),
    }
}

fn parse_video(s: &str) -> Result<Video> {
    match s {
        "webrtc" => Ok(Video::Webrtc),
        "jpeg" => Ok(Video::Jpeg),
        "none" => Ok(Video::None),
        _ => bail!("video jpeg|webrtc|none"),
    }
}

const XTERM_CSS: &str = include_str!("web/xterm.css");
const XTERM_JS: &str = include_str!("web/xterm.js");
const XTERM_FIT: &str = include_str!("web/xterm-addon-fit.js");

const INDEX: &str = r#"<!DOCTYPE html>
<meta charset="utf-8"><title>box</title>
<link rel="stylesheet" href="/xterm.css">
<body>
<p id="boot">loading…</p>
<script src="/xterm.js"></script>
<script src="/xterm-addon-fit.js"></script>
<script type="module">
  import init from "/pkg/demo_client_ui.js";
  try {
    await init({ module_or_path: "/pkg/demo_client_ui_bg.wasm" });
    document.getElementById("boot")?.remove();
  } catch (e) {
    document.getElementById("boot").textContent = String(e && e.message ? e.message : e);
  }
</script>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kind_video() {
        assert!(matches!(parse_kind("browser").unwrap(), Kind::Browser));
        assert!(matches!(parse_kind("shell").unwrap(), Kind::Shell));
        assert!(matches!(parse_kind("agent").unwrap(), Kind::Agent));
        assert!(parse_kind("nope").is_err());
        assert!(matches!(parse_video("jpeg").unwrap(), Video::Jpeg));
        assert!(matches!(parse_video("webrtc").unwrap(), Video::Webrtc));
        assert!(matches!(parse_video("none").unwrap(), Video::None));
        assert!(parse_video("gif").is_err());
    }

    #[test]
    fn ice_servers_stun_turn_and_fallback() {
        let v = ice_servers(&["stun:a:1".into(), "".into()], None, None, None);
        assert_eq!(v[0]["urls"][0], "stun:a:1");
        assert_eq!(v.as_array().unwrap().len(), 1);
        let v = ice_servers(&[], Some("turn:t:3478"), Some("u"), Some("p"));
        assert_eq!(v[0]["username"], "u");
        assert_eq!(v[0]["credential"], "p");
        let v = ice_servers(&[], None, None, None);
        assert_eq!(v[0]["urls"][0], "stun:stun.l.google.com:19302");
    }

    #[test]
    fn body_of_http() {
        let raw = b"POST /cmd HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}";
        let head = String::from_utf8_lossy(raw);
        assert_eq!(body_of(&head, raw), "{}");
        assert_eq!(body_of("no blank", b"no blank"), "");
    }

    #[test]
    fn resolve_and_peer_list() {
        let mut peers = HashMap::new();
        peers.insert("id-1".into(), "box-1".into());
        peers.insert("id-2".into(), "box-2".into());
        assert_eq!(resolve(&peers, "box-1").unwrap(), "id-1");
        assert_eq!(resolve(&peers, "id-2").unwrap(), "id-2");
        assert!(resolve(&peers, "missing").is_err());
        let list = peer_list(&peers);
        assert_eq!(list[0].1, "box-1");
        assert_eq!(list[1].1, "box-2");
    }

    #[test]
    fn remember_peer() {
        let mut peers = HashMap::new();
        remember(
            &mut peers,
            &Peer {
                id: "x".into(),
                name: "n".into(),
                ..Default::default()
            },
        );
        assert_eq!(peers.get("x").unwrap(), "n");
    }

    #[test]
    fn index_html_has_wasm_boot() {
        assert!(INDEX.contains("demo_client_ui.js"));
        assert!(INDEX.contains("demo_client_ui_bg.wasm"));
        assert!(INDEX.contains("xterm.js"));
    }
}
