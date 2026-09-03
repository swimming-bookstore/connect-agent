//! WebRTC media. Signaling is JSON on App. ICE: STUN punch, optional TURN.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use tokio::sync::mpsc;
use webrtc::api::interceptor_registry::{configure_nack, configure_rtcp_reports};
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_VP8};
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_credential_type::RTCIceCredentialType;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::media::Sample;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::policy::rtcp_mux_policy::RTCRtcpMuxPolicy;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType};
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;

use crate::protocol::Out;
use crate::video::{self, Encoder};

#[derive(Clone)]
pub struct Ice {
    servers: Vec<RTCIceServer>,
}

impl Ice {
    pub fn new(stun: &[String], turn: Option<&str>, user: Option<&str>, pass: Option<&str>) -> Self {
        let mut servers: Vec<RTCIceServer> = stun
            .iter()
            .filter(|u| !u.is_empty())
            .map(|urls| RTCIceServer {
                urls: vec![urls.clone()],
                ..Default::default()
            })
            .collect();
        if let Some(urls) = turn.filter(|s| !s.is_empty()) {
            servers.push(RTCIceServer {
                urls: vec![turn_url(urls)],
                username: user.unwrap_or("").into(),
                credential: pass.unwrap_or("").into(),
                credential_type: RTCIceCredentialType::Password,
                ..Default::default()
            });
        }
        if servers.is_empty() {
            servers.push(RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302".into()],
                ..Default::default()
            });
        }
        Self { servers }
    }
}

pub fn turn_url(urls: &str) -> String {
    let mut u = urls.to_string();
    if u.contains("127.0.0.1") || u.contains("localhost") {
        if let Some(ip) = local_ip() {
            let ip = ip.to_string();
            u = u.replace("127.0.0.1", &ip).replace("localhost", &ip);
        }
    }
    u
}

/// Browser iceServers. Chromium wants a string URL, no `?transport=`, and
/// localhost TURN when the fold is on the same box (LAN rewrite is the remote path).
pub fn browser_turn_urls(turn: &str) -> Vec<String> {
    let raw = turn.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let push = |v: &mut Vec<String>, s: String| {
        let s = s.split('?').next().unwrap_or(&s).to_string();
        if !s.is_empty() && !v.contains(&s) {
            v.push(s);
        }
    };
    push(&mut out, raw.into());
    push(&mut out, turn_url(raw));
    out
}

pub fn local_ip() -> Option<std::net::IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("1.1.1.1:80").ok()?;
    Some(sock.local_addr().ok()?.ip())
}

pub struct Rtc {
    pc: Arc<RTCPeerConnection>,
    encoder: Encoder,
}

impl Rtc {
    pub async fn answer(
        ice: Ice,
        width: u32,
        height: u32,
        offer: &str,
    ) -> Result<(Self, String, mpsc::Receiver<Out>)> {
        let encoder = Encoder::start(width, height)?;
        let mut media = MediaEngine::default();
        media.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: MIME_TYPE_VP8.to_owned(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line: String::new(),
                    rtcp_feedback: vec![],
                },
                payload_type: 96,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;
        let mut registry = Registry::new();
        registry = configure_nack(registry, &mut media);
        registry = configure_rtcp_reports(registry);
        let api = APIBuilder::new()
            .with_media_engine(media)
            .with_interceptor_registry(registry)
            .build();
        let pc = Arc::new(
            api.new_peer_connection(RTCConfiguration {
                ice_servers: ice.servers,
                rtcp_mux_policy: RTCRtcpMuxPolicy::Require,
                ..Default::default()
            })
            .await
            .context("peer connection")?,
        );

        let track = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_VP8.to_owned(),
                clock_rate: 90000,
                ..Default::default()
            },
            "video".into(),
            "connect".into(),
        ));
        pc.add_track(Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>)
            .await?;

        let (ice_tx, ice_rx) = mpsc::channel(64);
        pc.on_ice_candidate(Box::new(move |c| {
            let ice_tx = ice_tx.clone();
            Box::pin(async move {
                let Some(c) = c else { return };
                let Ok(init) = c.to_json() else { return };
                if init.candidate.split_whitespace().nth(1) == Some("2") {
                    return;
                }
                let mid = init
                    .sdp_mid
                    .filter(|s| !s.is_empty())
                    .or_else(|| Some("0".into()));
                let _ = ice_tx
                    .send(Out::Ice {
                        candidate: init.candidate,
                        sdp_mid: mid,
                        sdp_mline_index: init.sdp_mline_index.or(Some(0)),
                    })
                    .await;
            })
        }));

        pump_frames(encoder.subscribe(), Arc::clone(&track));

        pc.set_remote_description(RTCSessionDescription::offer(offer.to_string())?)
            .await
            .context("set remote offer")?;
        let answer = pc.create_answer(None).await.context("create answer")?;
        pc.set_local_description(answer).await?;
        let sdp = pc
            .local_description()
            .await
            .map(|d| strip_rtcp_candidates(d.sdp))
            .unwrap_or_default();
        tracing::info!(width, height, sdp_len = sdp.len(), "webrtc answer");
        Ok((Self { pc, encoder }, sdp, ice_rx))
    }

    pub fn push_jpeg(&self, jpeg: Vec<u8>) {
        self.encoder.push_jpeg(jpeg);
    }

    pub async fn add_ice(
        &self,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    ) -> Result<()> {
        if candidate.is_empty() {
            return Ok(());
        }
        self.pc
            .add_ice_candidate(RTCIceCandidateInit {
                candidate,
                sdp_mid,
                sdp_mline_index,
                username_fragment: None,
            })
            .await?;
        Ok(())
    }
}

fn strip_rtcp_candidates(sdp: String) -> String {
    sdp.lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("a=candidate:") && t.split_whitespace().nth(1) == Some("2"))
        })
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n"
}

impl Drop for Rtc {
    fn drop(&mut self) {
        let pc = Arc::clone(&self.pc);
        tokio::spawn(async move {
            let _ = pc.close().await;
        });
    }
}

fn pump_frames(mut frames: tokio::sync::broadcast::Receiver<Bytes>, track: Arc<TrackLocalStaticSample>) {
    let tick = Duration::from_nanos(1_000_000_000 / u64::from(video::FPS.max(1)));
    tokio::spawn(async move {
        loop {
            let data = match frames.recv().await {
                Ok(data) => data,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            if data.is_empty() {
                continue;
            }
            if track
                .write_sample(&Sample {
                    data,
                    duration: tick,
                    ..Default::default()
                })
                .await
                .is_err()
            {
                break;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_turn_urls_empty() {
        assert!(browser_turn_urls("").is_empty());
        assert!(browser_turn_urls("   ").is_empty());
    }

    #[test]
    fn browser_turn_urls_strips_query_and_dedupes() {
        let v = browser_turn_urls("turn:example.com:3478?transport=udp");
        assert_eq!(v, vec!["turn:example.com:3478".to_string()]);
    }

    #[test]
    fn browser_turn_urls_keeps_raw_and_rewritten_when_local() {
        let v = browser_turn_urls("turn:127.0.0.1:3478");
        assert!(v.iter().any(|u| u.contains("127.0.0.1") || u.contains("turn:")));
        assert_eq!(v[0], "turn:127.0.0.1:3478");
        if v.len() > 1 {
            assert!(!v[1].contains("127.0.0.1"));
            assert!(!v[1].contains("localhost"));
        }
    }

    #[test]
    fn strip_rtcp_component_2() {
        let sdp = [
            "v=0",
            "a=candidate:1 1 UDP 1 1.1.1.1 9 typ host",
            "a=candidate:1 2 UDP 1 1.1.1.1 10 typ host",
            "a=end-of-candidates",
        ]
        .join("\r\n")
            + "\r\n";
        let out = strip_rtcp_candidates(sdp);
        assert!(out.contains("a=candidate:1 1 UDP"));
        assert!(!out.contains("a=candidate:1 2 UDP"));
        assert!(out.contains("a=end-of-candidates"));
        assert!(out.ends_with("\r\n"));
    }

    #[test]
    fn ice_new_falls_back_to_google_stun() {
        let ice = Ice::new(&[], None, None, None);
        assert_eq!(ice.servers.len(), 1);
        assert_eq!(ice.servers[0].urls, vec!["stun:stun.l.google.com:19302"]);
    }

    #[test]
    fn ice_new_stun_and_turn() {
        let ice = Ice::new(
            &["stun:stun.example:3478".into(), "".into()],
            Some("turn:turn.example:3478"),
            Some("u"),
            Some("p"),
        );
        assert_eq!(ice.servers.len(), 2);
        assert_eq!(ice.servers[0].urls, vec!["stun:stun.example:3478"]);
        assert_eq!(ice.servers[1].username, "u");
        assert_eq!(ice.servers[1].credential, "p");
        assert_eq!(ice.servers[1].credential_type, RTCIceCredentialType::Password);
    }
}
