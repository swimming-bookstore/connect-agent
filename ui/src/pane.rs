//! Pane / tab helpers. No WASM so they unit-test natively.

pub fn parse_hash_pane(hash: &str) -> Option<String> {
    let h = hash.trim_start_matches('#');
    if h.is_empty() {
        return None;
    }
    match h {
        "browser" | "turn" => Some("jpeg".into()),
        "shell" | "jpeg" | "turn720" | "turn1080" | "agent" => Some(h.into()),
        _ => None,
    }
}

pub fn pane_from_kind(kind: &str, video: &str, height: u32) -> String {
    match kind {
        "browser" if video == "jpeg" => "jpeg".into(),
        "browser" if video == "webrtc" && height >= 1080 => "turn1080".into(),
        "browser" if video == "webrtc" => "turn720".into(),
        "agent" => "agent".into(),
        _ => "shell".into(),
    }
}

pub fn pane_want(p: &str) -> (&'static str, &'static str, u32) {
    match p {
        "jpeg" => ("browser", "jpeg", 720),
        "turn1080" => ("browser", "webrtc", 1080),
        "turn720" => ("browser", "webrtc", 720),
        "agent" => ("agent", "none", 0),
        _ => ("shell", "none", 0),
    }
}

pub fn tab_label(title: &str, url: &str) -> String {
    let title = title.trim();
    if !title.is_empty() {
        return title.into();
    }
    if url.is_empty() || url == "about:blank" {
        "about:blank".into()
    } else {
        String::new()
    }
}

pub fn box_name<'a>(peers: impl IntoIterator<Item = &'a str>) -> String {
    peers
        .into_iter()
        .next()
        .map(|n| n.to_string())
        .unwrap_or_else(|| "box-1".into())
}

pub fn wheel_scale(delta_mode: u32) -> f64 {
    match delta_mode {
        1 => 16.0,
        2 => 720.0,
        _ => 1.0,
    }
}

pub fn map_frame_xy(
    client_x: f64,
    client_y: f64,
    left: f64,
    top: f64,
    width: f64,
    height: f64,
    frame_w: f64,
    frame_h: f64,
) -> (f64, f64) {
    let x = (client_x - left) * (frame_w / width.max(1.0));
    let y = (client_y - top) * (frame_h / height.max(1.0));
    (x, y)
}

pub fn chat_row(line: &str) -> Option<(&'static str, String)> {
    if let Some(t) = line.strip_prefix("you ") {
        Some(("me", t.to_string()))
    } else if let Some(t) = line.strip_prefix("ai ") {
        Some(("bot", t.to_string()))
    } else if line.starts_with("step ") || line.starts_with("error ") {
        Some(("tool", line.to_string()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hash_pane_all() {
        assert_eq!(parse_hash_pane(""), None);
        assert_eq!(parse_hash_pane("#"), None);
        assert_eq!(parse_hash_pane("#browser"), Some("jpeg".into()));
        assert_eq!(parse_hash_pane("turn"), Some("jpeg".into()));
        assert_eq!(parse_hash_pane("#shell"), Some("shell".into()));
        assert_eq!(parse_hash_pane("jpeg"), Some("jpeg".into()));
        assert_eq!(parse_hash_pane("turn720"), Some("turn720".into()));
        assert_eq!(parse_hash_pane("turn1080"), Some("turn1080".into()));
        assert_eq!(parse_hash_pane("agent"), Some("agent".into()));
        assert_eq!(parse_hash_pane("nope"), None);
    }

    #[test]
    fn pane_from_kind_all() {
        assert_eq!(pane_from_kind("browser", "jpeg", 720), "jpeg");
        assert_eq!(pane_from_kind("browser", "webrtc", 720), "turn720");
        assert_eq!(pane_from_kind("browser", "webrtc", 1080), "turn1080");
        assert_eq!(pane_from_kind("agent", "none", 0), "agent");
        assert_eq!(pane_from_kind("shell", "none", 0), "shell");
        assert_eq!(pane_from_kind("", "", 0), "shell");
    }

    #[test]
    fn pane_want_all() {
        assert_eq!(pane_want("jpeg"), ("browser", "jpeg", 720));
        assert_eq!(pane_want("turn1080"), ("browser", "webrtc", 1080));
        assert_eq!(pane_want("turn720"), ("browser", "webrtc", 720));
        assert_eq!(pane_want("agent"), ("agent", "none", 0));
        assert_eq!(pane_want("shell"), ("shell", "none", 0));
        assert_eq!(pane_want("nope"), ("shell", "none", 0));
    }

    #[test]
    fn tab_label_rules() {
        assert_eq!(tab_label(" Example ", "https://x"), "Example");
        assert_eq!(tab_label("", "about:blank"), "about:blank");
        assert_eq!(tab_label("  ", ""), "about:blank");
        assert_eq!(tab_label("", "https://x"), "");
    }

    #[test]
    fn box_name_fallback() {
        assert_eq!(box_name(None), "box-1");
        assert_eq!(box_name(Some("box-9")), "box-9");
    }

    #[test]
    fn wheel_scale_modes() {
        assert_eq!(wheel_scale(0), 1.0);
        assert_eq!(wheel_scale(1), 16.0);
        assert_eq!(wheel_scale(2), 720.0);
        assert_eq!(wheel_scale(99), 1.0);
    }

    #[test]
    fn map_frame_xy_scales() {
        let (x, y) = map_frame_xy(10.0, 20.0, 0.0, 0.0, 640.0, 360.0, 1280.0, 720.0);
        assert_eq!((x, y), (20.0, 40.0));
        let (x, y) = map_frame_xy(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1280.0, 720.0);
        assert_eq!((x, y), (0.0, 0.0));
    }

    #[test]
    fn chat_row_kinds() {
        assert_eq!(chat_row("you hi"), Some(("me", "hi".into())));
        assert_eq!(chat_row("ai hello"), Some(("bot", "hello".into())));
        assert_eq!(chat_row("step read: ok"), Some(("tool", "step read: ok".into())));
        assert_eq!(chat_row("error boom"), Some(("tool", "error boom".into())));
        assert_eq!(chat_row("noise"), None);
    }
}
