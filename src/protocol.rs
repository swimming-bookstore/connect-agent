use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Browser,
    Shell,
    Agent,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Video {
    #[default]
    Webrtc,
    Jpeg,
    None,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum In {
    Open {
        #[serde(default)]
        kind: Kind,
        #[serde(default)]
        video: Video,
        #[serde(default)]
        height: u32,
    },
    Close,
    Click {
        x: f64,
        y: f64,
        #[serde(default)]
        button: u8,
    },
    Key {
        key: String,
        pressed: bool,
    },
    Navigate {
        url: String,
    },
    Eval {
        expression: String,
    },
    Wheel {
        x: f64,
        y: f64,
        #[serde(rename = "deltaX")]
        delta_x: f64,
        #[serde(rename = "deltaY")]
        delta_y: f64,
    },
    Back,
    Forward,
    NewTab {
        #[serde(default)]
        url: String,
    },
    CloseTab {
        id: String,
    },
    Focus {
        id: String,
    },
    Offer {
        sdp: String,
    },
    Ice {
        candidate: String,
        #[serde(default)]
        sdp_mid: Option<String>,
        #[serde(default)]
        sdp_mline_index: Option<u16>,
    },
    Stdin {
        data: String,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    AiUser {
        text: String,
    },
    /// Start a new Grok thread on the laptop. Box resets its turn state.
    AiNew,
    /// Laptop AI share: Grok result. Tokens never leave the laptop.
    AiResult {
        id: String,
        #[serde(default)]
        content: String,
        #[serde(default)]
        tool_calls: Vec<AiToolCall>,
        #[serde(default)]
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Out {
    Hello {
        kind: Kind,
        video: Video,
        width: u32,
        height: u32,
        fps: u32,
    },
    Answer {
        sdp: String,
    },
    Ice {
        candidate: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sdp_mid: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sdp_mline_index: Option<u16>,
    },
    Jpeg {
        data: String,
    },
    Tabs {
        tabs: Vec<Tab>,
        url: String,
    },
    Eval {
        result: String,
    },
    Stdout {
        data: String,
    },
    Exit {
        code: i32,
    },
    Error {
        message: String,
    },
    /// New Grok thread. Laptop drops history.
    AiChat {
        id: String,
    },
    /// Box coding agent: append these messages on the laptop, then complete.
    /// `reset` starts a new Grok thread (system+first user). Tokens stay on the laptop.
    AiAsk {
        id: String,
        #[serde(default)]
        reset: bool,
        append: Vec<serde_json::Value>,
    },
    AiReply {
        text: String,
    },
    AiStep {
        tool: String,
        args: serde_json::Value,
        result: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AiToolCall {
    pub id: String,
    pub name: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Tab {
    pub id: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    pub active: bool,
}

pub fn abs_url(s: &str) -> String {
    if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("about:") {
        s.into()
    } else {
        format!("https://{s}")
    }
}

impl In {
    /// Only `open` starts a session. Anything else with no live channel is dropped.
    pub fn start_session(self) -> Option<(Self, Kind, Video, u32)> {
        match self {
            In::Open { kind, video, height } => {
                let video = if kind == Kind::Browser {
                    video
                } else {
                    Video::None
                };
                Some((In::Open { kind, video, height }, kind, video, height))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abs_url_passthrough() {
        assert_eq!(abs_url("https://example.com"), "https://example.com");
        assert_eq!(abs_url("http://localhost:9"), "http://localhost:9");
        assert_eq!(abs_url("about:blank"), "about:blank");
    }

    #[test]
    fn abs_url_adds_https() {
        assert_eq!(abs_url("example.com"), "https://example.com");
        assert_eq!(abs_url("www.example.com/x"), "https://www.example.com/x");
    }

    #[test]
    fn kind_video_serde() {
        assert_eq!(serde_json::to_string(&Kind::Browser).unwrap(), "\"browser\"");
        assert_eq!(serde_json::to_string(&Kind::Shell).unwrap(), "\"shell\"");
        assert_eq!(serde_json::to_string(&Kind::Agent).unwrap(), "\"agent\"");
        assert_eq!(serde_json::to_string(&Video::Webrtc).unwrap(), "\"webrtc\"");
        assert_eq!(serde_json::to_string(&Video::Jpeg).unwrap(), "\"jpeg\"");
        assert_eq!(serde_json::to_string(&Video::None).unwrap(), "\"none\"");
        assert_eq!(serde_json::from_str::<Kind>("\"shell\"").unwrap(), Kind::Shell);
        assert_eq!(serde_json::from_str::<Video>("\"jpeg\"").unwrap(), Video::Jpeg);
    }

    #[test]
    fn in_open_roundtrip() {
        let v = serde_json::json!({"type":"open","kind":"browser","video":"jpeg","height":720});
        let msg: In = serde_json::from_value(v).unwrap();
        match msg {
            In::Open { kind, video, height } => {
                assert_eq!(kind, Kind::Browser);
                assert_eq!(video, Video::Jpeg);
                assert_eq!(height, 720);
            }
            _ => panic!("not open"),
        }
    }

    #[test]
    fn in_open_defaults() {
        let msg: In = serde_json::from_str(r#"{"type":"open"}"#).unwrap();
        match msg {
            In::Open { kind, video, height } => {
                assert_eq!(kind, Kind::Browser);
                assert_eq!(video, Video::Webrtc);
                assert_eq!(height, 0);
            }
            _ => panic!("not open"),
        }
    }

    #[test]
    fn in_navigate_click_key_wheel() {
        let nav: In = serde_json::from_str(r#"{"type":"navigate","url":"example.com"}"#).unwrap();
        assert!(matches!(nav, In::Navigate { url } if url == "example.com"));
        let click: In = serde_json::from_str(r#"{"type":"click","x":1.5,"y":2.5}"#).unwrap();
        match click {
            In::Click { x, y, button } => {
                assert_eq!((x, y, button), (1.5, 2.5, 0));
            }
            _ => panic!("not click"),
        }
        let key: In = serde_json::from_str(r#"{"type":"key","key":"a","pressed":true}"#).unwrap();
        assert!(matches!(key, In::Key { key, pressed } if key == "a" && pressed));
        let wheel: In =
            serde_json::from_str(r#"{"type":"wheel","x":0,"y":1,"deltaX":2,"deltaY":3}"#).unwrap();
        match wheel {
            In::Wheel {
                x,
                y,
                delta_x,
                delta_y,
            } => assert_eq!((x, y, delta_x, delta_y), (0.0, 1.0, 2.0, 3.0)),
            _ => panic!("not wheel"),
        }
    }

    #[test]
    fn in_tabs_and_nav() {
        let _: In = serde_json::from_str(r#"{"type":"back"}"#).unwrap();
        let _: In = serde_json::from_str(r#"{"type":"forward"}"#).unwrap();
        let _: In = serde_json::from_str(r#"{"type":"close"}"#).unwrap();
        let t: In = serde_json::from_str(r#"{"type":"new_tab"}"#).unwrap();
        assert!(matches!(t, In::NewTab { url } if url.is_empty()));
        let t: In = serde_json::from_str(r#"{"type":"close_tab","id":"t1"}"#).unwrap();
        assert!(matches!(t, In::CloseTab { id } if id == "t1"));
        let t: In = serde_json::from_str(r#"{"type":"focus","id":"t1"}"#).unwrap();
        assert!(matches!(t, In::Focus { id } if id == "t1"));
    }

    #[test]
    fn in_ai_and_shell() {
        let s: In = serde_json::from_str(r#"{"type":"stdin","data":"ls\n"}"#).unwrap();
        assert!(matches!(s, In::Stdin { data } if data == "ls\n"));
        let r: In = serde_json::from_str(r#"{"type":"resize","cols":80,"rows":24}"#).unwrap();
        assert!(matches!(r, In::Resize { cols: 80, rows: 24 }));
        let _: In = serde_json::from_str(r#"{"type":"ai_new"}"#).unwrap();
        let u: In = serde_json::from_str(r#"{"type":"ai_user","text":"hi"}"#).unwrap();
        assert!(matches!(u, In::AiUser { text } if text == "hi"));
        let res: In = serde_json::from_str(
            r#"{"type":"ai_result","id":"1","content":"ok","tool_calls":[],"error":null}"#,
        )
        .unwrap();
        assert!(matches!(res, In::AiResult { id, content, .. } if id == "1" && content == "ok"));
    }

    #[test]
    fn out_roundtrip() {
        let hello = Out::Hello {
            kind: Kind::Browser,
            video: Video::Jpeg,
            width: 1280,
            height: 720,
            fps: 8,
        };
        let v = serde_json::to_value(&hello).unwrap();
        assert_eq!(v["type"], "hello");
        assert_eq!(v["kind"], "browser");
        let tabs = Out::Tabs {
            tabs: vec![Tab {
                id: "a".into(),
                url: "about:blank".into(),
                title: "about:blank".into(),
                active: true,
            }],
            url: "about:blank".into(),
        };
        let back: Out = serde_json::from_value(serde_json::to_value(&tabs).unwrap()).unwrap();
        match back {
            Out::Tabs { tabs, url } => {
                assert_eq!(url, "about:blank");
                assert!(tabs[0].active);
            }
            _ => panic!("not tabs"),
        }
    }

    #[test]
    fn start_session_only_open() {
        assert!(In::Close.start_session().is_none());
        assert!(In::Back.start_session().is_none());
        let (msg, kind, video, h) = In::Open {
            kind: Kind::Browser,
            video: Video::Jpeg,
            height: 720,
        }
        .start_session()
        .unwrap();
        assert!(matches!(msg, In::Open { .. }));
        assert_eq!(kind, Kind::Browser);
        assert_eq!(video, Video::Jpeg);
        assert_eq!(h, 720);
    }

    #[test]
    fn start_session_non_browser_forces_no_video() {
        let (_, kind, video, _) = In::Open {
            kind: Kind::Shell,
            video: Video::Webrtc,
            height: 0,
        }
        .start_session()
        .unwrap();
        assert_eq!(kind, Kind::Shell);
        assert_eq!(video, Video::None);
        let (_, _, video, _) = In::Open {
            kind: Kind::Agent,
            video: Video::Jpeg,
            height: 1080,
        }
        .start_session()
        .unwrap();
        assert_eq!(video, Video::None);
    }

    #[test]
    fn ice_skips_empty_mid() {
        let out = Out::Ice {
            candidate: "cand".into(),
            sdp_mid: None,
            sdp_mline_index: Some(0),
        };
        let v = serde_json::to_value(&out).unwrap();
        assert!(v.get("sdp_mid").is_none());
        assert_eq!(v["sdp_mline_index"], 0);
    }
}
