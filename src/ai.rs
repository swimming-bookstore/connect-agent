//! Grok login on the laptop. Completions for the box coding agent run here.
//! Tokens default to `$XDG_DATA_HOME/fun/auth.json` (same file as fun-coding-agent).
//! Override with `PROVIDER_GROK_AUTH`.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::watch;

use crate::protocol::AiToolCall;

#[derive(Clone, Serialize)]
pub struct ChatInfo {
    pub configured: bool,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_uri: Option<String>,
}

fn pending() -> &'static watch::Sender<Option<(String, String)>> {
    static P: OnceLock<watch::Sender<Option<(String, String)>>> = OnceLock::new();
    P.get_or_init(|| watch::channel(None).0)
}

fn set_pending(v: Option<(String, String)>) {
    let _ = pending().send(v);
}

pub fn info() -> ChatInfo {
    let p = pending().borrow().clone();
    ChatInfo {
        configured: env_key().is_some() || provider_grok::has_tokens(),
        model: model_name(),
        user_code: p.as_ref().map(|(c, _)| c.clone()),
        verification_uri: p.as_ref().map(|(_, u)| u.clone()),
    }
}

pub async fn login_start() -> Result<ChatInfo> {
    let d = provider_grok::start_login().await?;
    let _ = open::that(&d.verification_uri_complete);
    set_pending(Some((
        d.user_code.clone(),
        d.verification_uri_complete.clone(),
    )));
    tokio::spawn(async move {
        let mut interval = d.interval;
        loop {
            if provider_grok::now_ms() >= d.deadline_ms {
                set_pending(None);
                break;
            }
            match provider_grok::poll_token(&d.device_code).await {
                Ok(provider_grok::Poll::Done) => {
                    set_pending(None);
                    break;
                }
                Ok(provider_grok::Poll::Pending) => {}
                Ok(provider_grok::Poll::SlowDown(n)) => interval = n,
                Ok(provider_grok::Poll::Denied | provider_grok::Poll::Expired) | Err(_) => {
                    set_pending(None);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    });
    Ok(info())
}

pub async fn logout() -> Result<ChatInfo> {
    let _ = provider_grok::logout().await;
    set_pending(None);
    Ok(info())
}

fn env_key() -> Option<String> {
    std::env::var("CONNECT_AI_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .ok()
        .filter(|s| !s.is_empty())
}

fn model_name() -> String {
    std::env::var("CONNECT_AI_MODEL").unwrap_or_else(|_| {
        if env_key().is_some() {
            "gpt-4o-mini".into()
        } else {
            "grok-4.6".into()
        }
    })
}

fn base_url(grok: bool) -> String {
    let mut base = std::env::var("CONNECT_AI_BASE")
        .or_else(|_| std::env::var("OPENAI_BASE_URL"))
        .unwrap_or_else(|_| {
            if grok {
                "https://api.x.ai/v1".into()
            } else {
                "https://api.openai.com/v1".into()
            }
        });
    while base.ends_with('/') {
        base.pop();
    }
    base
}

/// Call Grok on this laptop. Tokens never leave here.
pub async fn complete_local(messages: &Value, tools: &Value) -> Result<Completion> {
    let (token, base, model) = if let Some(key) = env_key() {
        (key, base_url(false), model_name())
    } else {
        let token = provider_grok::bearer()
            .await
            .context("not logged in — Login on this laptop")?;
        (token, base_url(true), model_name())
    };
    complete(&token, &base, &model, messages, tools).await
}

pub struct Completion {
    pub content: String,
    pub tool_calls: Vec<AiToolCall>,
}

async fn complete(
    token: &str,
    base: &str,
    model: &str,
    messages: &Value,
    tools: &Value,
) -> Result<Completion> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .build()?;
    let v: Value = http
        .post(format!("{base}/chat/completions"))
        .bearer_auth(token)
        .json(&json!({
            "model": model,
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto"
        }))
        .send()
        .await
        .context("ai http")?
        .error_for_status()
        .context("ai status")?
        .json()
        .await?;
    let msg = v
        .pointer("/choices/0/message")
        .cloned()
        .ok_or_else(|| anyhow!("ai: no message ({v})"))?;
    let content = msg["content"].as_str().unwrap_or("").to_string();
    let tool_calls = msg
        .get("tool_calls")
        .and_then(|c| c.as_array())
        .map(|calls| calls.iter().filter_map(parse_call).collect())
        .unwrap_or_default();
    Ok(Completion {
        content,
        tool_calls,
    })
}

fn parse_call(call: &Value) -> Option<AiToolCall> {
    let id = call.get("id")?.as_str()?.to_string();
    let name = call.pointer("/function/name")?.as_str()?.to_string();
    let raw = call.pointer("/function/arguments")?.as_str().unwrap_or("{}");
    let args = serde_json::from_str(raw).unwrap_or_else(|_| json!({}));
    Some(AiToolCall { id, name, args })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_call_ok() {
        let v = json!({
            "id": "c1",
            "function": {"name": "read", "arguments": "{\"path\":\"a.rs\"}"}
        });
        let c = parse_call(&v).unwrap();
        assert_eq!(c.id, "c1");
        assert_eq!(c.name, "read");
        assert_eq!(c.args["path"], "a.rs");
    }

    #[test]
    fn parse_call_bad_args_becomes_object() {
        let v = json!({
            "id": "c1",
            "function": {"name": "bash", "arguments": "not-json"}
        });
        let c = parse_call(&v).unwrap();
        assert_eq!(c.args, json!({}));
    }

    #[test]
    fn parse_call_missing_fields() {
        assert!(parse_call(&json!({"id":"x"})).is_none());
        assert!(parse_call(&json!({"function":{"name":"read"}})).is_none());
    }

    #[test]
    fn base_url_strips_slash() {
        std::env::set_var("CONNECT_AI_BASE", "https://api.example/v1/");
        assert_eq!(base_url(true), "https://api.example/v1");
        std::env::remove_var("CONNECT_AI_BASE");
        std::env::remove_var("OPENAI_BASE_URL");
        assert_eq!(base_url(true), "https://api.x.ai/v1");
        assert_eq!(base_url(false), "https://api.openai.com/v1");
    }

    #[test]
    fn model_name_default() {
        std::env::remove_var("CONNECT_AI_MODEL");
        std::env::remove_var("CONNECT_AI_KEY");
        std::env::remove_var("OPENAI_API_KEY");
        assert_eq!(model_name(), "grok-4.6");
        std::env::set_var("CONNECT_AI_MODEL", "custom");
        assert_eq!(model_name(), "custom");
        std::env::remove_var("CONNECT_AI_MODEL");
    }

    #[test]
    fn env_key_empty_is_none() {
        std::env::set_var("CONNECT_AI_KEY", "");
        std::env::remove_var("OPENAI_API_KEY");
        assert!(env_key().is_none());
        std::env::remove_var("CONNECT_AI_KEY");
    }
}
