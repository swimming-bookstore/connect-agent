//! JPEG only. Chromium screencast. No WebRTC.

use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use tokio::sync::mpsc;

use crate::chrome::Chromium;
use crate::pb::ClientMsg;
use crate::plane;
use crate::protocol::{In, Kind, Out, Video};
use crate::video;

pub async fn session(
    channel: String,
    dst: String,
    tx: mpsc::Sender<ClientMsg>,
    mut cmds: mpsc::Receiver<In>,
    idle: Duration,
    height: u32,
) {
    tracing::info!(%channel, "jpeg");
    let (w, h) = video::size(height);
    let chrome = match Chromium::spawn_unless_close(w, h, &mut cmds).await {
        Ok(Some(c)) => {
            tracing::info!(%channel, "jpeg up");
            c
        }
        Ok(None) => return,
        Err(e) => {
            tracing::error!("jpeg: {e:#}");
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
            video: Video::Jpeg,
            width: w,
            height: h,
            fps: 8,
        },
    )
    .await
    .is_err()
    {
        return;
    }
    let mut frames = chrome.subscribe();
    if let Some(jpeg) = chrome.last_jpeg() {
        if plane::send_out(
            &tx,
            &dst,
            &channel,
            &Out::Jpeg {
                data: STANDARD.encode(jpeg),
            },
        )
        .await
        .is_err()
        {
            return;
        }
    }
    let mut deadline = tokio::time::Instant::now() + idle;
    loop {
        tokio::select! {
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { break };
                if matches!(cmd, In::Close) { break; }
                if !idle.is_zero() {
                    deadline = tokio::time::Instant::now() + idle;
                }
                if let Some(msg) = chrome.apply(cmd).await {
                    if plane::send_out(&tx, &dst, &channel, &msg).await.is_err() {
                        break;
                    }
                }
            }
            jpeg = frames.recv() => match jpeg {
                Ok(jpeg) => {
                    if plane::send_out(&tx, &dst, &channel, &Out::Jpeg { data: STANDARD.encode(jpeg) }).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            _ = tokio::time::sleep_until(deadline), if !idle.is_zero() => break,
        }
    }
}
