use gloo_net::http::Request;
use gloo_timers::future::TimeoutFuture;
use js_sys::{Array, Reflect};
use serde_json::json;
use std::cell::RefCell;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    HtmlVideoElement, MediaStream, RtcConfiguration, RtcIceCandidateInit, RtcIceServer,
    RtcIceTransportPolicy, RtcPeerConnection, RtcPeerConnectionIceEvent, RtcRtpTransceiverDirection,
    RtcRtpTransceiverInit, RtcSdpType, RtcSessionDescriptionInit, RtcTrackEvent,
};

use crate::{IceCand, IceServer};

thread_local! {
    static PC: RefCell<Option<RtcPeerConnection>> = const { RefCell::new(None) };
    static HELLO: RefCell<(u32, u32)> = const { RefCell::new((0, 0)) };
    static ICE_N: RefCell<usize> = const { RefCell::new(0) };
    static GEN: RefCell<u32> = const { RefCell::new(0) };
    static BAD_ANSWER: RefCell<String> = const { RefCell::new(String::new()) };
    static STARTING: RefCell<bool> = const { RefCell::new(false) };
}

pub fn close() {
    GEN.with(|g| *g.borrow_mut() += 1);
    PC.with(|p| {
        if let Some(pc) = p.borrow_mut().take() {
            let _ = pc.close();
        }
    });
    HELLO.with(|h| *h.borrow_mut() = (0, 0));
    ICE_N.with(|n| *n.borrow_mut() = 0);
    BAD_ANSWER.with(|s| s.borrow_mut().clear());
    STARTING.with(|s| *s.borrow_mut() = false);
    if let Some(w) = web_sys::window() {
        if let Some(d) = w.document() {
            if let Some(el) = d.get_element_by_id("rtc") {
                if let Ok(v) = el.dyn_into::<HtmlVideoElement>() {
                    let _ = v.pause();
                    v.set_src_object(None);
                }
            }
        }
    }
}

pub async fn apply(
    width: u32,
    height: u32,
    answer: &str,
    ice: &[IceCand],
    servers: &[IceServer],
    video: HtmlVideoElement,
) {
    let have = HELLO.with(|h| *h.borrow());
    if have != (width, height) {
        if STARTING.with(|s| *s.borrow()) {
            return;
        }
        let gen = GEN.with(|g| {
            *g.borrow_mut() += 1;
            *g.borrow()
        });
        PC.with(|p| {
            if let Some(pc) = p.borrow_mut().take() {
                let _ = pc.close();
            }
        });
        ICE_N.with(|n| *n.borrow_mut() = 0);
        HELLO.with(|h| *h.borrow_mut() = (width, height));
        BAD_ANSWER.with(|s| s.borrow_mut().clear());
        STARTING.with(|s| *s.borrow_mut() = true);
        let started = start(servers, video, gen).await;
        STARTING.with(|s| *s.borrow_mut() = false);
        match started {
            Ok(pc) => {
                if GEN.with(|g| *g.borrow() != gen) {
                    let _ = pc.close();
                    return;
                }
                PC.with(|p| *p.borrow_mut() = Some(pc));
            }
            Err(e) => {
                if GEN.with(|g| *g.borrow() != gen) {
                    return;
                }
                log(&format!("webrtc: {e}"));
                HELLO.with(|h| *h.borrow_mut() = (0, 0));
                return;
            }
        }
    }
    if !answer.is_empty() && BAD_ANSWER.with(|s| *s.borrow() != answer) {
        let applied = PC.with(|p| p.borrow().as_ref().cloned());
        if let Some(pc) = applied {
            if pc.remote_description().is_none() {
                if let Err(e) = set_remote_answer(&pc, answer).await {
                    log(&format!("answer: {e}"));
                    BAD_ANSWER.with(|s| *s.borrow_mut() = answer.to_string());
                    return;
                }
            }
        }
    }
    let from = ICE_N.with(|n| *n.borrow());
    if from < ice.len() {
        let applied = PC.with(|p| {
            if let Some(pc) = p.borrow().as_ref() {
                if pc.remote_description().is_none() {
                    return false;
                }
                for c in &ice[from..] {
                    add_remote_ice(pc, c);
                }
                true
            } else {
                false
            }
        });
        if applied {
            ICE_N.with(|n| *n.borrow_mut() = ice.len());
        }
    }
}

fn add_remote_ice(pc: &RtcPeerConnection, c: &IceCand) {
    if c.candidate.is_empty() {
        return;
    }
    let cand = if c.candidate.starts_with("candidate:") || c.candidate == "null" {
        c.candidate.clone()
    } else {
        format!("candidate:{}", c.candidate)
    };
    let init = RtcIceCandidateInit::new(&cand);
    let mid = c
        .sdp_mid
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("0");
    init.set_sdp_mid(Some(mid));
    init.set_sdp_m_line_index(Some(c.sdp_mline_index.unwrap_or(0)));
    let _ = pc.add_ice_candidate_with_opt_rtc_ice_candidate_init(Some(&init));
}

async fn start(
    servers: &[IceServer],
    video: HtmlVideoElement,
    gen: u32,
) -> Result<RtcPeerConnection, String> {
    let cfg = RtcConfiguration::new();
    let arr = Array::new();
    for s in servers {
        let ice = RtcIceServer::new();
        let urls = Array::new();
        for u in &s.urls {
            urls.push(&JsValue::from_str(u));
        }
        ice.set_urls(&urls);
        if !s.username.is_empty() {
            ice.set_username(&s.username);
            ice.set_credential(&s.credential);
        }
        arr.push(&ice);
    }
    if arr.length() == 0 {
        let ice = RtcIceServer::new();
        ice.set_urls(&JsValue::from_str("stun:stun.l.google.com:19302"));
        arr.push(&ice);
    }
    cfg.set_ice_servers(&arr);
    cfg.set_ice_transport_policy(RtcIceTransportPolicy::All);
    let pc = RtcPeerConnection::new_with_configuration(&cfg).map_err(js_err)?;

    let tr = RtcRtpTransceiverInit::new();
    tr.set_direction(RtcRtpTransceiverDirection::Recvonly);
    let _ = pc.add_transceiver_with_str_and_init("video", &tr);

    let vid = video.clone();
    let on_track = Closure::<dyn FnMut(RtcTrackEvent)>::new(move |ev: RtcTrackEvent| {
        if GEN.with(|g| *g.borrow() != gen) {
            return;
        }
        let stream = ev
            .streams()
            .get(0)
            .dyn_into::<MediaStream>()
            .ok()
            .or_else(|| {
                let s = MediaStream::new().ok()?;
                s.add_track(&ev.track());
                Some(s)
            });
        if let Some(stream) = stream {
            attach(&vid, &stream);
        }
    });
    pc.set_ontrack(Some(on_track.as_ref().unchecked_ref()));
    on_track.forget();

    let on_ice = Closure::<dyn FnMut(RtcPeerConnectionIceEvent)>::new(
        move |ev: RtcPeerConnectionIceEvent| {
            if GEN.with(|g| *g.borrow() != gen) {
                return;
            }
            let Some(c) = ev.candidate() else {
                return;
            };
            let candidate = c.candidate();
            if candidate.is_empty() {
                return;
            }
            if candidate.split_whitespace().nth(1) == Some("2") {
                return;
            }
            let mid = c.sdp_mid().filter(|m| !m.is_empty()).or(Some("0".into()));
            let idx = c.sdp_m_line_index().or(Some(0));
            wasm_bindgen_futures::spawn_local(async move {
                if GEN.with(|g| *g.borrow() != gen) {
                    return;
                }
                post(json!({
                    "type": "ice",
                    "candidate": candidate,
                    "sdp_mid": mid,
                    "sdp_mline_index": idx,
                }))
                .await;
            });
        },
    );
    pc.set_onicecandidate(Some(on_ice.as_ref().unchecked_ref()));
    on_ice.forget();

    let offer = JsFuture::from(pc.create_offer())
        .await
        .map_err(js_err)?;
    js_set_local(&pc, &offer).await?;
    let mut sdp = desc_sdp(&pc.local_description().map(|d| d.into()).unwrap_or(JsValue::NULL));
    if !has_ufrag(&sdp) {
        sdp = desc_sdp(&offer);
    }
    for _ in 0..80 {
        if GEN.with(|g| *g.borrow() != gen) {
            let _ = pc.close();
            return Err("stale".into());
        }
        if has_ufrag(&sdp) {
            break;
        }
        TimeoutFuture::new(50).await;
        sdp = desc_sdp(&pc.local_description().map(|d| d.into()).unwrap_or(JsValue::NULL));
        if !has_ufrag(&sdp) {
            sdp = desc_sdp(&offer);
        }
    }
    if GEN.with(|g| *g.borrow() != gen) {
        let _ = pc.close();
        return Err("stale".into());
    }
    if !has_ufrag(&sdp) {
        return Err(format!(
            "offer missing ice-ufrag (len={} head={:?})",
            sdp.len(),
            sdp.chars().take(80).collect::<String>()
        ));
    }
    post(json!({"type": "offer", "sdp": sdp})).await;
    Ok(pc)
}

async fn set_remote_answer(pc: &RtcPeerConnection, sdp: &str) -> Result<(), String> {
    let desc = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
    desc.set_sdp(sdp);
    JsFuture::from(pc.set_remote_description(&desc))
        .await
        .map_err(js_err)?;
    Ok(())
}

fn attach(video: &HtmlVideoElement, stream: &MediaStream) {
    video.set_muted(true);
    video.set_autoplay(true);
    let _ = video.set_attribute("playsinline", "");
    let _ = video.set_attribute("webkit-playsinline", "");
    video.set_src_object(Some(stream));
    let v = video.clone();
    wasm_bindgen_futures::spawn_local(async move {
        for _ in 0..20 {
            match v.play() {
                Ok(p) => {
                    if JsFuture::from(p).await.is_ok() {
                        return;
                    }
                }
                Err(_) => {}
            }
            TimeoutFuture::new(200).await;
        }
    });
}

async fn js_set_local(pc: &RtcPeerConnection, desc: &JsValue) -> Result<(), String> {
    let f = Reflect::get(pc.as_ref(), &"setLocalDescription".into()).map_err(js_err)?;
    let f = f.dyn_into::<js_sys::Function>().map_err(js_err)?;
    let p = f.call1(pc.as_ref(), desc).map_err(js_err)?;
    if p.has_type::<js_sys::Promise>() {
        JsFuture::from(js_sys::Promise::from(p))
            .await
            .map_err(js_err)?;
    }
    Ok(())
}

fn desc_sdp(v: &JsValue) -> String {
    if v.is_null() || v.is_undefined() {
        return String::new();
    }
    if let Some(s) = Reflect::get(v, &"sdp".into())
        .ok()
        .and_then(|x| x.as_string())
    {
        if !s.is_empty() {
            return s;
        }
    }
    if let Ok(js) = js_sys::JSON::stringify(v) {
        if let Some(s) = js.as_string() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(sdp) = v.get("sdp").and_then(|x| x.as_str()) {
                    return sdp.to_string();
                }
            }
            return s;
        }
    }
    String::new()
}

fn has_ufrag(sdp: &str) -> bool {
    let s = sdp.to_ascii_lowercase();
    s.contains("a=ice-ufrag") && s.contains("a=ice-pwd")
}

async fn post(v: serde_json::Value) {
    let Ok(req) = Request::post("/cmd")
        .header("content-type", "application/json")
        .body(v.to_string())
    else {
        return;
    };
    let _ = req.send().await;
}

fn js_err(v: wasm_bindgen::JsValue) -> String {
    v.as_string()
        .or_else(|| {
            Reflect::get(&v, &"message".into())
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| "js error".into())
}

fn log(msg: &str) {
    if let Some(w) = web_sys::window() {
        let _ = js_sys::Reflect::get(&w, &"console".into())
            .ok()
            .and_then(|c| {
                let f = js_sys::Reflect::get(&c, &"error".into()).ok()?;
                let f = f.dyn_into::<js_sys::Function>().ok()?;
                let _ = f.call1(&c, &msg.into());
                Some(())
            });
    }
}
