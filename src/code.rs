//! Minimal coding agent on the box. Tools run here. Completions come from Alice's AI share.

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

use crate::pb::ClientMsg;
use crate::plane;
use crate::protocol::{AiToolCall, In, Kind, Out, Video};

const MAX_ROUNDS: usize = 24;
const MAX_READ: usize = 50 * 1024;
const MAX_BASH: usize = 20_000;

pub async fn session(
    channel: String,
    dst: String,
    tx: mpsc::Sender<ClientMsg>,
    mut cmds: mpsc::Receiver<In>,
    idle: Duration,
) {
    tracing::info!(%channel, "coding agent on box");
    let chat = new_id();
    if plane::send_out(
        &tx,
        &dst,
        &channel,
        &Out::Hello {
            kind: Kind::Agent,
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
    if plane::send_out(&tx, &dst, &channel, &Out::AiChat { id: chat }).await.is_err() {
        return;
    }
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut pending: Option<String> = None;
    let mut queued: Vec<String> = Vec::new();
    let mut fresh = true;
    let mut rounds: usize = 0;
    let mut deadline = tokio::time::Instant::now() + idle;
    loop {
        tokio::select! {
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { break };
                if matches!(cmd, In::Close) { break; }
                if !idle.is_zero() { deadline = tokio::time::Instant::now() + idle; }
                match cmd {
                    In::AiNew => {
                        pending = None;
                        queued.clear();
                        fresh = true;
                        rounds = 0;
                        if plane::send_out(&tx, &dst, &channel, &Out::AiChat { id: new_id() }).await.is_err() {
                            break;
                        }
                    }
                    In::AiUser { text } => {
                        if pending.is_some() {
                            queued.push(text);
                            continue;
                        }
                        rounds = 0;
                        let reset = fresh;
                        let append = first_or_user(&root, fresh, text);
                        fresh = false;
                        if let Err(e) = ask_share(&tx, &channel, &dst, append, reset, &mut pending).await {
                            if plane::send_out(&tx, &dst, &channel, &Out::Error { message: e.to_string() }).await.is_err() {
                                break;
                            }
                        }
                    }
                    In::AiResult { id, content, tool_calls, error } => {
                        if pending.as_deref() != Some(id.as_str()) {
                            continue;
                        }
                        pending = None;
                        if let Some(err) = error {
                            if plane::send_out(&tx, &dst, &channel, &Out::Error { message: err }).await.is_err() {
                                break;
                            }
                            if drain_queue(&tx, &channel, &dst, &root, &mut queued, &mut pending, &mut fresh, &mut rounds).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        if tool_calls.is_empty() {
                            if plane::send_out(&tx, &dst, &channel, &Out::AiReply { text: content }).await.is_err() {
                                break;
                            }
                            if drain_queue(&tx, &channel, &dst, &root, &mut queued, &mut pending, &mut fresh, &mut rounds).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        rounds += 1;
                        if rounds > MAX_ROUNDS {
                            if plane::send_out(&tx, &dst, &channel, &Out::AiReply { text: "done.".into() }).await.is_err() {
                                break;
                            }
                            if drain_queue(&tx, &channel, &dst, &root, &mut queued, &mut pending, &mut fresh, &mut rounds).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        let mut append = vec![json!({
                            "role": "assistant",
                            "content": content,
                            "tool_calls": tool_calls.iter().map(|c| json!({
                                "id": c.id,
                                "type": "function",
                                "function": { "name": c.name, "arguments": c.args.to_string() }
                            })).collect::<Vec<_>>(),
                        })];
                        let mut dead = false;
                        for call in tool_calls {
                            let result = run_tool(&root, &call).await;
                            if plane::send_out(&tx, &dst, &channel, &Out::AiStep {
                                tool: call.name.clone(),
                                args: call.args.clone(),
                                result: clip(&result, 8 * 1024),
                            }).await.is_err() {
                                dead = true;
                                break;
                            }
                            append.push(json!({
                                "role": "tool",
                                "tool_call_id": call.id,
                                "content": clip(&result, 24 * 1024),
                            }));
                        }
                        if dead {
                            break;
                        }
                        if let Err(e) = ask_share(&tx, &channel, &dst, append, false, &mut pending).await {
                            if plane::send_out(&tx, &dst, &channel, &Out::Error { message: e.to_string() }).await.is_err() {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ = tokio::time::sleep_until(deadline), if !idle.is_zero() => break,
        }
    }
}

fn first_or_user(root: &Path, fresh: bool, text: String) -> Vec<Value> {
    if fresh {
        vec![
            json!({"role": "system", "content": system_prompt(root)}),
            json!({"role": "user", "content": text}),
        ]
    } else {
        vec![json!({"role": "user", "content": text})]
    }
}

async fn drain_queue(
    tx: &mpsc::Sender<ClientMsg>,
    channel: &str,
    dst: &str,
    root: &Path,
    queued: &mut Vec<String>,
    pending: &mut Option<String>,
    fresh: &mut bool,
    rounds: &mut usize,
) -> Result<()> {
    let Some(text) = queued.first().cloned() else {
        return Ok(());
    };
    queued.remove(0);
    *rounds = 0;
    let reset = *fresh;
    let append = first_or_user(root, *fresh, text);
    *fresh = false;
    if let Err(e) = ask_share(tx, channel, dst, append, reset, pending).await {
        plane::send_out(tx, dst, channel, &Out::Error { message: e.to_string() }).await?;
    }
    Ok(())
}

async fn ask_share(
    tx: &mpsc::Sender<ClientMsg>,
    channel: &str,
    dst: &str,
    append: Vec<Value>,
    reset: bool,
    pending: &mut Option<String>,
) -> Result<()> {
    if pending.is_some() {
        return Ok(());
    }
    let id = new_id();
    *pending = Some(id.clone());
    plane::send_out(
        tx,
        dst,
        channel,
        &Out::AiAsk {
            id,
            reset,
            append,
        },
    )
    .await
}

fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    format!("{:x}", N.fetch_add(1, Ordering::Relaxed))
}

fn system_prompt(root: &Path) -> String {
    format!(
        "You are a coding agent on this box. Workspace: {}. \
         Tools (read/write/edit/bash) run here. Completions are provided by the laptop AI share. \
         Be concise. Do not invent tool results.",
        root.display()
    )
}

pub fn coding_tools() -> Value {
    json!([
        fn_tool("read", "Read a file. A directory lists names.", json!({
            "path": {"type":"string"},
            "offset": {"type":"integer"},
            "limit": {"type":"integer"}
        }), &["path"]),
        fn_tool("write", "Write a file (overwrite).", json!({
            "path": {"type":"string"},
            "content": {"type":"string"}
        }), &["path","content"]),
        fn_tool("edit", "Replace old with new. old must appear exactly once.", json!({
            "path": {"type":"string"},
            "old": {"type":"string"},
            "new": {"type":"string"}
        }), &["path","old","new"]),
        fn_tool("bash", "Run a shell command in the workspace.", json!({
            "cmd": {"type":"string"}
        }), &["cmd"]),
    ])
}

fn fn_tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": properties,
                "required": required
            }
        }
    })
}

async fn run_tool(root: &Path, call: &AiToolCall) -> String {
    match call.name.as_str() {
        "read" => read_file(root, &call.args),
        "write" => write_file(root, &call.args),
        "edit" => edit_file(root, &call.args),
        "bash" => bash(root, &call.args).await,
        other => format!("unknown tool {other}"),
    }
}

fn arg<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v.get(k).and_then(Value::as_str).ok_or_else(|| anyhow!("missing {k}"))
}

fn resolve(root: &Path, path: &str) -> Result<PathBuf> {
    if path.is_empty() {
        bail!("empty path");
    }
    let requested = Path::new(path);
    if requested.is_absolute() {
        bail!("path must be inside workspace");
    }
    let root = clean(root);
    let candidate = clean(&root.join(requested));
    if !candidate.starts_with(&root) {
        bail!("path must be inside workspace");
    }
    Ok(candidate)
}

fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !out.has_root() {
                    out.push(c);
                }
            }
            c => out.push(c),
        }
    }
    out
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}\n… truncated", &s[..max])
    }
}

fn read_file(root: &Path, args: &Value) -> String {
    let path = match arg(args, "path").and_then(|p| resolve(root, p)) {
        Ok(p) => p,
        Err(e) => return e.to_string(),
    };
    if path.is_dir() {
        return match fs::read_dir(&path) {
            Ok(rd) => rd
                .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                .collect::<Vec<_>>()
                .join("\n"),
            Err(e) => e.to_string(),
        };
    }
    let Ok(bytes) = fs::read(&path) else {
        return format!("read {}", path.display());
    };
    if bytes.contains(&0) {
        return format!("binary {} bytes={}", path.display(), bytes.len());
    }
    let text = String::from_utf8_lossy(&bytes);
    let start = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let want = args.get("limit").and_then(Value::as_u64).unwrap_or(500) as usize;
    let mut out = String::new();
    let mut seen = 0usize;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if i < start {
            continue;
        }
        if seen >= want {
            break;
        }
        let line = line.trim_end_matches(['\n', '\r']);
        let row = format!("{}\t{line}\n", i + 1);
        if out.len() + row.len() > MAX_READ {
            out.push_str("… truncated\n");
            break;
        }
        out.push_str(&row);
        seen += 1;
    }
    if out.is_empty() {
        "(empty)".into()
    } else {
        out
    }
}

fn write_file(root: &Path, args: &Value) -> String {
    let path = match arg(args, "path").and_then(|p| resolve(root, p)) {
        Ok(p) => p,
        Err(e) => return e.to_string(),
    };
    let content = match arg(args, "content") {
        Ok(s) => s,
        Err(e) => return e.to_string(),
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    match fs::write(&path, content) {
        Ok(()) => format!("wrote {}", path.display()),
        Err(e) => e.to_string(),
    }
}

fn edit_file(root: &Path, args: &Value) -> String {
    let path = match arg(args, "path").and_then(|p| resolve(root, p)) {
        Ok(p) => p,
        Err(e) => return e.to_string(),
    };
    let old = match arg(args, "old") {
        Ok(s) => s,
        Err(e) => return e.to_string(),
    };
    let new = match arg(args, "new") {
        Ok(s) => s,
        Err(e) => return e.to_string(),
    };
    let Ok(text) = fs::read_to_string(&path) else {
        return format!("read {}", path.display());
    };
    let n = text.matches(old).count();
    if n != 1 {
        return format!("old must appear exactly once (found {n})");
    }
    match fs::write(&path, text.replacen(old, new, 1)) {
        Ok(()) => format!("edited {}", path.display()),
        Err(e) => e.to_string(),
    }
}

async fn bash(root: &Path, args: &Value) -> String {
    let cmd = match arg(args, "cmd") {
        Ok(s) => s.to_string(),
        Err(e) => return e.to_string(),
    };
    let mut child = match tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return e.to_string(),
    };
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(s) = stdout.as_mut() {
            let _ = s.read_to_end(&mut buf).await;
        }
        buf
    });
    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(s) = stderr.as_mut() {
            let _ = s.read_to_end(&mut buf).await;
        }
        buf
    });
    let status = match tokio::time::timeout(Duration::from_secs(30), child.wait()).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return e.to_string(),
        Err(_) => {
            let _ = child.start_kill();
            return "bash timed out".into();
        }
    };
    let mut text = String::from_utf8_lossy(&out_task.await.unwrap_or_default()).into_owned();
    text.push_str(&String::from_utf8_lossy(&err_task.await.unwrap_or_default()));
    if text.len() > MAX_BASH {
        text.truncate(MAX_BASH);
        text.push_str("\n… truncated");
    }
    if text.is_empty() {
        text = "(no output)".into();
    }
    if status.success() {
        text
    } else {
        format!("exit {status}: {text}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "connect-agent-code-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn resolve_rejects_escape_and_absolute() {
        let root = tmp();
        assert!(resolve(&root, "").is_err());
        assert!(resolve(&root, "/etc/passwd").is_err());
        assert!(resolve(&root, "../outside").is_err());
        let ok = resolve(&root, "src/lib.rs").unwrap();
        assert!(ok.starts_with(&root));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clean_parent_dir() {
        let p = clean(Path::new("a/b/../c"));
        assert_eq!(p, PathBuf::from("a/c"));
        let p = clean(Path::new("./x"));
        assert_eq!(p, PathBuf::from("x"));
    }

    #[test]
    fn clip_truncates() {
        assert_eq!(clip("ab", 5), "ab");
        let s = clip("abcdef", 3);
        assert!(s.starts_with("abc"));
        assert!(s.contains("truncated"));
    }

    #[test]
    fn read_write_edit() {
        let root = tmp();
        let w = write_file(
            &root,
            &json!({"path": "note.txt", "content": "hello world"}),
        );
        assert!(w.contains("wrote"));
        let r = read_file(&root, &json!({"path": "note.txt"}));
        assert!(r.contains("hello world"));
        let e = edit_file(
            &root,
            &json!({"path": "note.txt", "old": "world", "new": "box"}),
        );
        assert!(e.contains("edited"));
        let r = read_file(&root, &json!({"path": "note.txt"}));
        assert!(r.contains("hello box"));
        let bad = edit_file(
            &root,
            &json!({"path": "note.txt", "old": "nope", "new": "x"}),
        );
        assert!(bad.contains("exactly once"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn read_dir_and_offset() {
        let root = tmp();
        fs::write(root.join("a.txt"), "1\n2\n3\n").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/b.txt"), "x").unwrap();
        let listing = read_file(&root, &json!({"path": "."}));
        assert!(listing.contains("a.txt"));
        assert!(listing.contains("sub"));
        let slice = read_file(&root, &json!({"path": "a.txt", "offset": 1, "limit": 1}));
        assert!(slice.contains("2"));
        assert!(!slice.contains("3"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn read_binary() {
        let root = tmp();
        fs::write(root.join("bin"), [0u8, 1, 2]).unwrap();
        let r = read_file(&root, &json!({"path": "bin"}));
        assert!(r.contains("binary"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn first_or_user_fresh() {
        let root = Path::new("/ws");
        let fresh = first_or_user(root, true, "hi".into());
        assert_eq!(fresh.len(), 2);
        assert_eq!(fresh[0]["role"], "system");
        assert!(fresh[0]["content"].as_str().unwrap().contains("/ws"));
        assert_eq!(fresh[1]["content"], "hi");
        let later = first_or_user(root, false, "more".into());
        assert_eq!(later.len(), 1);
        assert_eq!(later[0]["role"], "user");
    }

    #[test]
    fn coding_tools_names() {
        let t = coding_tools();
        let names: Vec<_> = t
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["read", "write", "edit", "bash"]);
    }

    #[test]
    fn unknown_tool() {
        let root = tmp();
        let r = tokio::runtime::Runtime::new().unwrap().block_on(run_tool(
            &root,
            &AiToolCall {
                id: "1".into(),
                name: "nope".into(),
                args: json!({}),
            },
        ));
        assert!(r.contains("unknown tool"));
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn bash_echo() {
        let root = tmp();
        let r = bash(&root, &json!({"cmd": "echo hi"})).await;
        assert!(r.contains("hi"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn new_id_increments() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
    }
}
