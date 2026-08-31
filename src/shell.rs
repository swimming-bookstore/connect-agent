//! Interactive PTY. Bytes ride the plane App JSON.

use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::process::Stdio;

use anyhow::{anyhow, Context, Result};
use nix::pty::{openpty, Winsize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

pub struct Shell {
    child: Child,
    writer: tokio::fs::File,
}

impl Shell {
    pub fn spawn() -> Result<(Self, mpsc::Receiver<Vec<u8>>)> {
        let ws = Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&ws), None).context("openpty")?;
        let master_raw = pty.master.as_raw_fd();
        let slave_raw = pty.slave.as_raw_fd();

        let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        let mut cmd = Command::new(&sh);
        cmd.arg("-i")
            .env("TERM", "xterm-256color")
            .stdin(dup_stdio(slave_raw)?)
            .stdout(dup_stdio(slave_raw)?)
            .stderr(dup_stdio(slave_raw)?)
            .kill_on_drop(true);
        unsafe {
            cmd.pre_exec(move || {
                nix::unistd::setsid()?;
                libc::ioctl(slave_raw, libc::TIOCSCTTY, 0);
                Ok(())
            });
        }
        let child = cmd.spawn().with_context(|| format!("spawn {sh}"))?;
        drop(pty.slave);

        let read_fd = nix::unistd::dup(master_raw).context("dup master")?;
        let reader = unsafe { std::fs::File::from_raw_fd(read_fd.into_raw_fd()) };
        let writer = unsafe { std::fs::File::from_raw_fd(pty.master.into_raw_fd()) };
        let mut reader = tokio::fs::File::from_std(reader);
        let writer = tokio::fs::File::from_std(writer);
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 8192];
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok((Self { child, writer }, rx))
    }

    pub async fn write(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data).await?;
        let _ = self.writer.flush().await;
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let ws = libc::winsize {
            ws_row: rows.max(1),
            ws_col: cols.max(1),
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe { libc::ioctl(self.writer.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
        if rc < 0 {
            return Err(anyhow!("TIOCSWINSZ"));
        }
        Ok(())
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

fn dup_stdio(fd: i32) -> Result<Stdio> {
    let n = nix::unistd::dup(fd).context("dup pty")?;
    Ok(unsafe { Stdio::from(std::fs::File::from_raw_fd(n.into_raw_fd())) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_echo_and_resize() {
        let (mut sh, mut rx) = Shell::spawn().expect("pty");
        sh.resize(100, 30).expect("resize");
        sh.write(b"printf hi-from-pty\n").await.expect("write");
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut got = Vec::new();
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(chunk)) =
                tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
            {
                got.extend_from_slice(&chunk);
                if String::from_utf8_lossy(&got).contains("hi-from-pty") {
                    return;
                }
            }
        }
        panic!("no pty output: {:?}", String::from_utf8_lossy(&got));
    }
}
