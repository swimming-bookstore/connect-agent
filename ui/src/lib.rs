mod pane;
mod rtc;
mod xterm;

use gloo_net::http::Request;
use gloo_timers::future::TimeoutFuture;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use serde_json::json;
use std::cell::RefCell;
use wasm_bindgen::JsCast;
use web_sys::{HtmlInputElement, HtmlVideoElement};

thread_local! {
    static TERM: RefCell<Option<xterm::Term>> = const { RefCell::new(None) };
    static SEEN: RefCell<usize> = const { RefCell::new(0) };
    static FRAME: RefCell<(f64, f64)> = const { RefCell::new((1280.0, 720.0)) };
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct Peer {
    id: String,
    name: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct IceCand {
    #[serde(default)]
    candidate: String,
    #[serde(default)]
    sdp_mid: Option<String>,
    #[serde(default)]
    sdp_mline_index: Option<u16>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct IceServer {
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub credential: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct Tab {
    id: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    active: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct Grok {
    #[serde(default)]
    configured: bool,
    #[serde(default)]
    model: String,
    #[serde(default)]
    user_code: Option<String>,
    #[serde(default)]
    verification_uri: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct State {
    #[serde(default)]
    peers: Vec<Peer>,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    video: String,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    #[serde(default)]
    answer: String,
    #[serde(default)]
    ice: Vec<IceCand>,
    #[serde(default)]
    ice_servers: Vec<IceServer>,
    #[serde(default)]
    url: String,
    #[serde(default)]
    tabs: Vec<Tab>,
    #[serde(default)]
    stdout: String,
    #[serde(default)]
    log: Vec<String>,
    #[serde(default)]
    grok: Grok,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

async fn cmd(v: serde_json::Value) {
    let Ok(req) = Request::post("/cmd")
        .header("content-type", "application/json")
        .body(v.to_string())
    else {
        return;
    };
    let _ = req.send().await;
}

fn box_name(s: &State) -> String {
    pane::box_name(s.peers.iter().map(|p| p.name.as_str()))
}

fn hash_pane() -> Option<String> {
    web_sys::window()
        .and_then(|w| w.location().hash().ok())
        .and_then(|h| pane::parse_hash_pane(&h))
}

fn set_hash(p: &str) {
    if let Some(w) = web_sys::window() {
        let _ = w.location().set_hash(p);
    }
}

fn pane_from_state(s: &State) -> String {
    pane::pane_from_kind(&s.kind, &s.video, s.height)
}

fn pane_want(p: &str) -> (&'static str, &'static str, u32) {
    pane::pane_want(p)
}

fn tab_label(title: &str, url: &str) -> String {
    pane::tab_label(title, url)
}

#[component]
fn App() -> impl IntoView {
    let pane = RwSignal::new(hash_pane().unwrap_or_else(|| "shell".into()));
    let st = RwSignal::new(State::default());
    let draft = RwSignal::new(String::new());
    let url_edit = RwSignal::new(String::new());
    let url_typing = RwSignal::new(false);
    let tick = RwSignal::new(0u32);
    let shot = RwSignal::new(String::new());
    let last_kind = RwSignal::new(String::new());
    let term_ref = NodeRef::<leptos::html::Div>::new();
    let url_ref = NodeRef::<leptos::html::Input>::new();
    let live_ref = NodeRef::<leptos::html::Video>::new();
    Effect::new(move |_| {
        let s = st.get();
        let h = if s.height >= 1080 { 1080.0 } else { 720.0 };
        let w = if s.height >= 1080 { 1920.0 } else { 1280.0 };
        FRAME.with(|f| *f.borrow_mut() = (w, h));
        let u = s
            .tabs
            .iter()
            .find(|t| t.active)
            .map(|t| t.url.clone())
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| s.url.clone());
        if !url_typing.get_untracked() {
            url_edit.set(u);
        }
    });
    let go = move || {
        url_typing.set(false);
        let u = url_ref
            .get()
            .map(|el| el.value())
            .unwrap_or_else(|| url_edit.get_untracked());
        spawn_local(async move {
            cmd(json!({"type":"navigate","url": u})).await;
        });
    };

    Effect::new(move |_| {
        spawn_local(async move {
            loop {
                if let Ok(r) = Request::get("/state").send().await {
                    if let Ok(s) = r.json::<State>().await {
                        st.set(s);
                    }
                }
                tick.update(|n| *n = n.wrapping_add(1));
                TimeoutFuture::new(120).await;
            }
        });
    });

    Effect::new(move |_| {
        let Some(el) = term_ref.get() else {
            return;
        };
        TERM.with(|t| {
            if t.borrow().is_some() {
                return;
            }
            if let Ok(term) = xterm::Term::mount(&el) {
                term.on_data(|data| {
                    spawn_local(async move {
                        cmd(json!({"type":"stdin","data": data})).await;
                    });
                });
                *t.borrow_mut() = Some(term);
            } else if let Some(w) = web_sys::window() {
                let _ = js_sys::Reflect::get(&w, &"console".into()).ok().and_then(|c| {
                    let f = js_sys::Reflect::get(&c, &"error".into()).ok()?;
                    let f = f.dyn_into::<js_sys::Function>().ok()?;
                    let _ = f.call1(&c, &"xterm mount failed".into());
                    Some(())
                });
            }
        });
    });

    Effect::new(move |_| {
        let s = st.get();
        if s.kind != last_kind.get_untracked() {
            if s.kind == "shell" {
                TERM.with(|t| {
                    if let Some(term) = t.borrow().as_ref() {
                        term.reset();
                    }
                });
                SEEN.with(|n| *n.borrow_mut() = 0);
            }
            last_kind.set(s.kind.clone());
        }
        SEEN.with(|seen| {
            let mut n = seen.borrow_mut();
            if s.stdout.len() < *n {
                TERM.with(|t| {
                    if let Some(term) = t.borrow().as_ref() {
                        term.reset();
                    }
                });
                *n = 0;
            }
            if s.stdout.len() > *n {
                let chunk = s.stdout[*n..].to_string();
                TERM.with(|t| {
                    if let Some(term) = t.borrow().as_ref() {
                        term.write(&chunk);
                    }
                });
                *n = s.stdout.len();
            }
        });
    });

    Effect::new(move |_| {
        let s = st.get();
        let Some(el) = live_ref.get() else {
            return;
        };
        let video: HtmlVideoElement = el;
        let turn = pane.get();
        if turn != "turn720" && turn != "turn1080" {
            return;
        }
        if s.kind != "browser" || s.video != "webrtc" || s.width == 0 {
            return;
        }
        if turn == "turn1080" && s.height < 1080 {
            return;
        }
        if turn == "turn720" && s.height >= 1080 {
            return;
        }
        let answer = s.answer.clone();
        let ice = s.ice.clone();
        let servers = s.ice_servers.clone();
        let w = s.width;
        let h = s.height;
        spawn_local(async move {
            rtc::apply(w, h, &answer, &ice, &servers, video).await;
        });
    });

    let opened = RwSignal::new(false);
    Effect::new(move |_| {
        let s = st.get();
        if opened.get_untracked() {
            return;
        }
        if s.peers.is_empty() {
            return;
        }
        opened.set(true);
        let p = hash_pane().unwrap_or_else(|| pane_from_state(&s));
        pane.set(p.clone());
        set_hash(&p);
        let (want_kind, want_video, want_h) = pane_want(&p);
        let same = s.kind == want_kind
            && (want_kind != "browser" || s.video == want_video)
            && (want_kind != "browser" || !p.starts_with("turn") || s.height == want_h);
        // Ctrl+Shift+R: WASM is new, box PC is old. Always re-open TURN.
        if !same || p.starts_with("turn") {
            rtc::close();
            let dst = box_name(&s);
            spawn_local(async move {
                cmd(json!({"type":"open","dst": dst, "kind": want_kind, "video": want_video, "height": want_h})).await;
            });
        }
    });

    let open_pane = move |p: &'static str| {
        pane.set(p.into());
        set_hash(p);
        let s = st.get_untracked();
        let (want_kind, want_video, want_h) = pane_want(p);
        let same = s.kind == want_kind
            && (want_kind != "browser" || s.video == want_video)
            && (!p.starts_with("turn") || s.height == want_h);
        if !same {
            rtc::close();
            let dst = box_name(&s);
            spawn_local(async move {
                cmd(json!({"type":"open","dst": dst, "kind": want_kind, "video": want_video, "height": want_h})).await;
            });
        }
        if p == "shell" {
            TERM.with(|t| {
                if let Some(term) = t.borrow().as_ref() {
                    term.fit();
                    term.focus();
                    let (cols, rows) = term.size();
                    spawn_local(async move {
                        cmd(json!({"type":"resize","cols": cols, "rows": rows})).await;
                    });
                }
            });
        }
    };

    view! {
        <style>{include_str!("style.css")}</style>
        <aside id="rail">
            <nav id="nav">
                <button data-testid="nav-shell" class:on=move || pane.get() == "shell" on:click=move |_| open_pane("shell")>"Shell"</button>
                <button data-testid="nav-jpeg" class:on=move || pane.get() == "jpeg" on:click=move |_| open_pane("jpeg")>"Browser JPEG"</button>
                <button data-testid="nav-turn720" class:on=move || pane.get() == "turn720" on:click=move |_| open_pane("turn720")>"Browser TURN 720p"</button>
                <button data-testid="nav-turn1080" class:on=move || pane.get() == "turn1080" on:click=move |_| open_pane("turn1080")>"Browser TURN 1080p"</button>
                <button data-testid="nav-agent" class:on=move || pane.get() == "agent" on:click=move |_| open_pane("agent")>"Agent"</button>
            </nav>
            <div id="share">
                <span id="model">{move || st.get().grok.model}</span>
                <button id="login" type="button" on:click=move |_| {
                    let on = st.get_untracked().grok.configured;
                    spawn_local(async move {
                        cmd(json!({"type": if on { "logout" } else { "login" }})).await;
                    });
                }>{move || {
                    let g = st.get().grok;
                    if g.user_code.is_some() { "Waiting" }
                    else if g.configured { "Logout" }
                    else { "Login" }
                }}</button>
            </div>
        </aside>
        <div id="stage-wrap">
            {move || st.get().grok.user_code.map(|code| {
                let href = st.get().grok.verification_uri.unwrap_or_else(|| "#".into());
                view! {
                    <div id="gate">
                        <span>"Grok login on this laptop. Completions stay here; the box only sends asks."</span>
                        <span id="usercode">{code}</span>
                        <a id="verify" href=href target="_blank">"open x.com"</a>
                    </div>
                }
            })}
            <div id="chrome" class:off=move || { let p = pane.get(); p != "jpeg" && p != "turn720" && p != "turn1080" }>
                <div id="tabs">
                    <For
                        each=move || st.get().tabs
                        key=|t| format!("{}|{}|{}", t.id, t.title, t.url)
                        children=move |t| {
                            let id = t.id.clone();
                            let id2 = t.id.clone();
                            view! {
                                <button class="btab" data-testid="tab" class:on=t.active
                                    on:mousedown=move |_| {
                                        let id = id.clone();
                                        spawn_local(async move { cmd(json!({"type":"focus","id": id})).await; });
                                    }>
                                    <span data-testid="tab-label">{tab_label(&t.title, &t.url)}</span>
                                    <span class="x" on:mousedown=move |ev| {
                                        ev.stop_propagation();
                                        let id = id2.clone();
                                        spawn_local(async move { cmd(json!({"type":"close_tab","id": id})).await; });
                                    }>"×"</span>
                                </button>
                            }
                        }
                    />
                    <button class="btab" data-testid="tab-new" type="button" on:click=move |_| {
                        spawn_local(async { cmd(json!({"type":"new_tab","url":"about:blank"})).await; });
                    }>"+"</button>
                </div>
                <div id="bar">
                    <button id="back" data-testid="nav-back" type="button" on:click=move |_| {
                        spawn_local(async { cmd(json!({"type":"back"})).await; });
                    }>"←"</button>
                    <button id="fwd" data-testid="nav-fwd" type="button" on:click=move |_| {
                        spawn_local(async { cmd(json!({"type":"forward"})).await; });
                    }>"→"</button>
                    <input id="url" type="text" node_ref=url_ref
                        prop:value=move || url_edit.get()
                        on:focus=move |_| url_typing.set(true)
                        on:blur=move |_| url_typing.set(false)
                        on:input=move |ev| {
                            url_typing.set(true);
                            if let Some(el) = ev.target().and_then(|t| t.dyn_into::<HtmlInputElement>().ok()) {
                                url_edit.set(el.value());
                            }
                        }
                        on:keydown=move |ev: web_sys::KeyboardEvent| {
                            if ev.key() == "Enter" {
                                go();
                            }
                        } />
                    <button id="go" type="button" on:click=move |_| go()>"Go"</button>
                </div>
            </div>
            <div class="pane" class:off=move || pane.get() != "jpeg" id="stage">
                <img id="view" tabindex="0" src=move || shot.get() alt=""
                    on:mousedown=move |ev| browser_click(&ev)
                    on:wheel=move |ev| browser_wheel(&ev)
                    on:keydown=move |ev| browser_key(&ev, true)
                    on:keyup=move |ev| browser_key(&ev, false)
                />
                <img id="view-next" src=move || if pane.get() == "jpeg" { format!("/shot?{}", tick.get()) } else { String::new() } alt=""
                    on:load=move |ev| {
                        if let Some(el) = ev.target().and_then(|t| t.dyn_into::<web_sys::HtmlImageElement>().ok()) {
                            if el.natural_width() < 32 {
                                return;
                            }
                            let u = el.src();
                            if !u.is_empty() {
                                shot.set(u);
                            }
                        }
                    }
                />
            </div>
            <div class="pane" class:off=move || { let p = pane.get(); p != "turn720" && p != "turn1080" } id="live">
                <video id="rtc" node_ref=live_ref autoplay muted playsinline tabindex="0"
                    on:mousedown=move |ev| browser_click(&ev)
                    on:wheel=move |ev| browser_wheel(&ev)
                    on:keydown=move |ev| browser_key(&ev, true)
                    on:keyup=move |ev| browser_key(&ev, false)
                ></video>
            </div>
            <div class="pane" class:off=move || pane.get() != "shell" id="term" node_ref=term_ref></div>
            <div class="pane" class:off=move || pane.get() != "agent" id="agent">
                <div id="agent-top">
                    <button type="button" id="newchat" on:click=move |_| {
                        spawn_local(async { cmd(json!({"type":"new_chat"})).await; });
                    }>"New chat"</button>
                </div>
                <div id="chat">
                    {move || st.get().log.into_iter().filter_map(|line| {
                        pane::chat_row(&line).map(|(kind, t)| {
                            view! { <div class=format!("row {kind}")>{t}</div> }.into_any()
                        })
                    }).collect_view()}
                </div>
                <form id="ask" on:submit=move |ev| {
                    ev.prevent_default();
                    let t = draft.get();
                    draft.set(String::new());
                    if !t.is_empty() {
                        spawn_local(async move { cmd(json!({"type":"ask","text": t})).await; });
                    }
                }>
                    <input id="q" prop:value=move || draft.get()
                        on:input=move |ev| {
                            if let Some(el) = ev.target().and_then(|t| t.dyn_into::<HtmlInputElement>().ok()) {
                                draft.set(el.value());
                            }
                        }
                        placeholder="Message the box coding agent" />
                    <button>"Send"</button>
                </form>
            </div>
        </div>
    }
}

fn map_xy(target: Option<web_sys::EventTarget>, client_x: i32, client_y: i32) -> Option<(f64, f64)> {
    let el = target?.dyn_into::<web_sys::Element>().ok()?;
    let _ = el.dyn_ref::<web_sys::HtmlElement>().map(|e| e.focus());
    let r = el.get_bounding_client_rect();
    let (fw, fh) = FRAME.with(|f| *f.borrow());
    let x = (client_x as f64 - r.left()) * (fw / r.width().max(1.0));
    let y = (client_y as f64 - r.top()) * (fh / r.height().max(1.0));
    Some((x, y))
}

fn browser_click(ev: &web_sys::MouseEvent) {
    let Some((x, y)) = map_xy(ev.current_target(), ev.client_x(), ev.client_y()) else {
        return;
    };
    spawn_local(async move { cmd(json!({"type":"click","x": x, "y": y})).await; });
}

fn browser_wheel(ev: &web_sys::WheelEvent) {
    ev.prevent_default();
    let Some((x, y)) = map_xy(ev.current_target(), ev.client_x(), ev.client_y()) else {
        return;
    };
    let scale = pane::wheel_scale(ev.delta_mode());
    let dx = ev.delta_x() * scale;
    let dy = ev.delta_y() * scale;
    spawn_local(async move {
        cmd(json!({"type":"wheel","x": x, "y": y, "deltaX": dx, "deltaY": dy})).await;
    });
}

fn browser_key(ev: &web_sys::KeyboardEvent, pressed: bool) {
    if ev.ctrl_key() || ev.meta_key() || ev.alt_key() {
        return;
    }
    ev.prevent_default();
    let key = ev.key();
    spawn_local(async move { cmd(json!({"type":"key","key": key, "pressed": pressed})).await; });
}
