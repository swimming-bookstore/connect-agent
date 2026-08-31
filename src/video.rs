//! JPEG in, IVF VP8 frames out. ffmpeg on PATH (or FFMPEG=).

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, oneshot};

pub const FPS: u32 = 24;

pub fn size(height: u32) -> (u32, u32) {
    if height >= 1080 {
        (1920, 1080)
    } else {
        (1280, 720)
    }
}

pub struct Encoder {
    jpeg_tx: mpsc::UnboundedSender<Vec<u8>>,
    nals: broadcast::Sender<Bytes>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl Encoder {
    pub fn start(width: u32, height: u32) -> Result<Self> {
        let bin = std::env::var("FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
        let fps = FPS.to_string();
        let w = width & !1;
        let h = height & !1;
        let scale = format!("scale={w}:{h}");
        let bitrate = if h >= 1080 { "2500k" } else { "1800k" };
        let buf = if h >= 1080 { "500k" } else { "400k" };
        let mut child = Command::new(&bin)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-fflags",
                "nobuffer+flush_packets",
                "-flags",
                "low_delay",
                "-thread_queue_size",
                "1",
                "-f",
                "mjpeg",
                "-use_wallclock_as_timestamps",
                "1",
                "-i",
                "pipe:0",
                "-an",
                "-c:v",
                "libvpx",
                "-deadline",
                "realtime",
                "-cpu-used",
                "5",
                "-error-resilient",
                "1",
                "-auto-alt-ref",
                "0",
                "-lag-in-frames",
                "0",
                "-quality",
                "realtime",
                "-undershoot-pct",
                "95",
                "-overshoot-pct",
                "15",
                "-pix_fmt",
                "yuv420p",
                "-g",
                "12",
                "-keyint_min",
                "12",
                "-b:v",
                bitrate,
                "-maxrate",
                bitrate,
                "-bufsize",
                buf,
                "-vf",
                &scale,
                "-r",
                &fps,
                "-f",
                "ivf",
                "pipe:1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn {bin} (set FFMPEG=)"))?;
        tracing::info!(bin, width = w, height = h, "ffmpeg encoder");

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("ffmpeg stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("ffmpeg stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("ffmpeg stderr"))?;
        let (jpeg_tx, jpeg_rx) = mpsc::unbounded_channel();
        let (nals, _) = broadcast::channel(64);
        let (shutdown, shutdown_rx) = oneshot::channel();
        tokio::spawn(run(child, jpeg_rx, stdin, stdout, stderr, nals.clone(), shutdown_rx));
        let enc = Self {
            jpeg_tx,
            nals,
            shutdown: Some(shutdown),
        };
        Ok(enc)
    }

    pub fn push_jpeg(&self, jpeg: Vec<u8>) {
        let _ = self.jpeg_tx.send(jpeg);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Bytes> {
        self.nals.subscribe()
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

async fn run(
    mut child: Child,
    mut jpeg_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    mut stdin: tokio::process::ChildStdin,
    mut stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    nals: broadcast::Sender<Bytes>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    tokio::spawn(async move {
        let mut stderr = BufReader::new(stderr);
        let mut line = String::new();
        while let Ok(n) = stderr.read_line(&mut line).await {
            if n == 0 {
                break;
            }
            tracing::warn!(ffmpeg = %line.trim(), "encoder");
            line.clear();
        }
    });

    let pump_in = async {
        let mut last: Option<Vec<u8>> = None;
        let mut tick = tokio::time::interval(Duration::from_millis(1000 / FPS.max(1) as u64));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                jpeg = jpeg_rx.recv() => {
                    let Some(mut jpeg) = jpeg else { break };
                    while let Ok(next) = jpeg_rx.try_recv() {
                        jpeg = next;
                    }
                    last = Some(jpeg);
                }
                _ = tick.tick() => {}
            }
            let Some(jpeg) = last.as_ref() else { continue };
            if stdin.write_all(jpeg).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
        drop(stdin);
    };
    let pump_out = async {
        let mut buf = vec![0u8; 65536];
        let mut pending = Vec::new();
        let mut nframes = 0u32;
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    pending.extend_from_slice(&buf[..n]);
                    while let Some(frame) = take_ivf(&mut pending) {
                        nframes += 1;
                        if nframes == 1 || nframes % 48 == 0 {
                            tracing::info!(nframes, bytes = frame.len(), "vp8");
                        }
                        let _ = nals.send(frame);
                    }
                }
            }
        }
        tracing::info!(nframes, "encoder stdout closed");
    };

    tokio::select! {
        _ = &mut shutdown_rx => {
            let _ = child.start_kill();
        }
        _ = async {
            tokio::join!(pump_in, pump_out);
        } => {}
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

fn take_ivf(buf: &mut Vec<u8>) -> Option<Bytes> {
    // skip file header once
    if buf.len() >= 32 && &buf[..4] == b"DKIF" {
        buf.drain(..32);
    }
    if buf.len() < 12 {
        return None;
    }
    let size = u32::from_le_bytes(buf[0..4].try_into().ok()?) as usize;
    let need = 12 + size;
    if buf.len() < need {
        return None;
    }
    let frame = Bytes::copy_from_slice(&buf[12..need]);
    buf.drain(..need);
    if frame.is_empty() {
        None
    } else {
        Some(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_720_and_1080() {
        assert_eq!(size(0), (1280, 720));
        assert_eq!(size(719), (1280, 720));
        assert_eq!(size(720), (1280, 720));
        assert_eq!(size(1079), (1280, 720));
        assert_eq!(size(1080), (1920, 1080));
        assert_eq!(size(2160), (1920, 1080));
    }

    fn ivf_frame(payload: &[u8], ts: u64) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        v.extend_from_slice(&ts.to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn take_ivf_skips_dkif_header() {
        let mut buf = b"DKIF".to_vec();
        buf.extend_from_slice(&[0u8; 28]);
        buf.extend(ivf_frame(b"vp8frame!", 1));
        let frame = take_ivf(&mut buf).unwrap();
        assert_eq!(&frame[..], b"vp8frame!");
        assert!(buf.is_empty());
        assert!(take_ivf(&mut buf).is_none());
    }

    #[test]
    fn take_ivf_waits_for_full_frame() {
        let full = ivf_frame(&[1, 2, 3, 4], 9);
        let mut buf = full[..8].to_vec();
        assert!(take_ivf(&mut buf).is_none());
        buf.extend_from_slice(&full[8..]);
        assert_eq!(&take_ivf(&mut buf).unwrap()[..], &[1, 2, 3, 4]);
    }

    #[test]
    fn take_ivf_drops_empty_payload() {
        let mut buf = ivf_frame(&[], 0);
        assert!(take_ivf(&mut buf).is_none());
        assert!(buf.is_empty());
    }

    #[test]
    fn take_ivf_two_frames() {
        let mut buf = ivf_frame(b"aa", 1);
        buf.extend(ivf_frame(b"bbb", 2));
        assert_eq!(&take_ivf(&mut buf).unwrap()[..], b"aa");
        assert_eq!(&take_ivf(&mut buf).unwrap()[..], b"bbb");
        assert!(buf.is_empty());
    }
}
