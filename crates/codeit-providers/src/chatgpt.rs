//! OpenAI through a ChatGPT Plus/Pro subscription instead of an API key, the way opencode's
//! built-in Codex login works: OpenAI's OAuth (in the browser, or with a device code), then the
//! Codex /responses endpoint with the access token.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::UnboundedSender;

use crate::copilot::models::{CopilotModel, Endpoint};
use crate::paths::debug;
use crate::{ChatEvent, ChatRequest, DeviceLogin, protocols};

/// The Codex CLI's public OAuth client, which opencode uses too.
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const ISSUER: &str = "https://auth.openai.com";
const ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const PORT: u16 = 1455;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn user_agent() -> String {
    format!("codeit/{}", env!("CARGO_PKG_VERSION"))
}

pub fn logged_in() -> bool {
    crate::auth::get("openai").is_some_and(|(v, _)| v["type"] == "oauth")
}

/// Models a ChatGPT login may use: the same rule as opencode (GPT-5.5 and later, a few named ones).
pub fn allowed(id: &str) -> bool {
    const ALLOWED: &[&str] = &["gpt-5.5", "gpt-5.3-codex-spark", "gpt-5.4", "gpt-5.4-mini", "gpt-6-sol", "gpt-6-luna"];
    if ALLOWED.contains(&id) {
        return true;
    }
    if id.ends_with("-pro") || id == "gpt-5.6" {
        return false;
    }
    let Some(version) = id.strip_prefix("gpt-").and_then(|r| r.split('-').next()) else { return false };
    let mut parts = version.split('.');
    let (Some(Ok(major)), minor) = (parts.next().map(str::parse::<u32>), parts.next().map(str::parse::<u32>)) else {
        return false;
    };
    let minor = match minor {
        None => 0,
        Some(Ok(m)) => m,
        Some(Err(_)) => return false,
    };
    major > 5 || (major == 5 && minor > 4)
}

// ── tokens ──────────────────────────────────────────────────────────────────

fn b64url(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (*b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn b64url_decode(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'-' | b'+' => 62,
        _ => 63,
    };
    let bytes: Vec<u8> = s.bytes().filter(|c| *c != b'=').collect();
    let mut out = Vec::new();
    for chunk in bytes.chunks(4) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, c)| acc | (val(*c) as u32) << (18 - 6 * i));
        for i in 0..chunk.len().saturating_sub(1) {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    out
}

/// The ChatGPT account a token belongs to.
fn account_id(token: &str) -> Option<String> {
    let claims: Value = serde_json::from_slice(&b64url_decode(token.split('.').nth(1)?)).ok()?;
    claims["chatgpt_account_id"]
        .as_str()
        .or(claims["https://api.openai.com/auth"]["chatgpt_account_id"].as_str())
        .or(claims["organizations"][0]["id"].as_str())
        .map(String::from)
}

fn save(tokens: &Value) -> Result<()> {
    let access = tokens["access_token"].as_str().ok_or_else(|| anyhow!("OpenAI sent no access token"))?;
    let account = tokens["id_token"].as_str().and_then(account_id).or_else(|| account_id(access));
    let mut entry = json!({
        "type": "oauth",
        "access": access,
        "refresh": tokens["refresh_token"],
        "expires": now_ms() + tokens["expires_in"].as_u64().unwrap_or(3600) * 1000,
    });
    if let Some(a) = account {
        entry["accountId"] = json!(a);
    }
    crate::auth::set("openai", entry)
}

async fn token_request(form: &[(&str, &str)]) -> Result<Value> {
    let res = crate::http().post(format!("{ISSUER}/oauth/token")).form(form).send().await?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("OpenAI login failed ({status}): {}", crate::snippet(&text));
    }
    Ok(serde_json::from_str(&text)?)
}

/// A current access token and account, refreshed when expired.
async fn access() -> Result<(String, Option<String>)> {
    let (v, _) = crate::auth::get("openai").ok_or_else(|| anyhow!("Not logged in to ChatGPT. Run `/login openai`."))?;
    let access = v["access"].as_str().unwrap_or_default().to_string();
    let account = v["accountId"].as_str().map(String::from);
    if !access.is_empty() && v["expires"].as_u64().unwrap_or(0) > now_ms() + 60_000 {
        return Ok((access, account));
    }
    let refresh = v["refresh"].as_str().ok_or_else(|| anyhow!("The ChatGPT login expired. Run `/login openai`."))?;
    let tokens =
        token_request(&[("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", CLIENT_ID)]).await?;
    let mut tokens = tokens;
    if tokens["refresh_token"].is_null() {
        tokens["refresh_token"] = json!(refresh);
    }
    save(&tokens)?;
    let (v, _) = crate::auth::get("openai").unwrap();
    Ok((v["access"].as_str().unwrap_or_default().to_string(), v["accountId"].as_str().map(String::from).or(account)))
}

// ── logins ──────────────────────────────────────────────────────────────────

/// A started login: show `login` (a URL, and a code to enter there when there is one), then wait.
pub struct Flow {
    pub login: DeviceLogin,
    wait: std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>,
}

impl Flow {
    pub async fn wait(self) -> Result<()> {
        self.wait.await
    }
}

/// Login with a code entered at auth.openai.com (works without a local browser).
pub async fn device() -> Result<Flow> {
    let res = crate::http()
        .post(format!("{ISSUER}/api/accounts/deviceauth/usercode"))
        .header("user-agent", user_agent())
        .json(&json!({ "client_id": CLIENT_ID }))
        .send()
        .await?;
    if !res.status().is_success() {
        bail!("OpenAI refused to start a device login ({})", res.status());
    }
    let d: Value = res.json().await?;
    let id = d["device_auth_id"].as_str().unwrap_or_default().to_string();
    let code = d["user_code"].as_str().unwrap_or_default().to_string();
    let interval = d["interval"].as_str().and_then(|i| i.parse().ok()).or(d["interval"].as_u64()).unwrap_or(5).max(1);
    let login = DeviceLogin { url: format!("{ISSUER}/codex/device"), code: code.clone() };
    let wait = Box::pin(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15 * 60);
        loop {
            tokio::time::sleep(Duration::from_secs(interval + 3)).await;
            if tokio::time::Instant::now() > deadline {
                bail!("the login code expired");
            }
            let res = crate::http()
                .post(format!("{ISSUER}/api/accounts/deviceauth/token"))
                .header("user-agent", user_agent())
                .json(&json!({ "device_auth_id": id, "user_code": code }))
                .send()
                .await?;
            match res.status().as_u16() {
                200 => {
                    let r: Value = res.json().await?;
                    let tokens = token_request(&[
                        ("grant_type", "authorization_code"),
                        ("code", r["authorization_code"].as_str().unwrap_or_default()),
                        ("redirect_uri", &format!("{ISSUER}/deviceauth/callback")),
                        ("client_id", CLIENT_ID),
                        ("code_verifier", r["code_verifier"].as_str().unwrap_or_default()),
                    ])
                    .await?;
                    return save(&tokens);
                }
                403 | 404 => continue,
                s => bail!("the login failed ({s})"),
            }
        }
    });
    Ok(Flow { login, wait })
}

/// Login in the browser, which comes back to `http://localhost:1455/auth/callback`.
pub async fn browser() -> Result<Flow> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", PORT))
        .await
        .map_err(|e| anyhow!("port {PORT} is busy ({e}); use the device code login instead"))?;
    let verifier = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
    let state = uuid::Uuid::new_v4().simple().to_string();
    let redirect = format!("http://localhost:{PORT}/auth/callback");
    let enc = |s: &str| s.replace(':', "%3A").replace('/', "%2F").replace(' ', "%20");
    let url = format!(
        "{ISSUER}/oauth/authorize?response_type=code&client_id={CLIENT_ID}&redirect_uri={}&scope={}&code_challenge={challenge}&code_challenge_method=S256&id_token_add_organizations=true&codex_cli_simplified_flow=true&state={state}&originator=codeit",
        enc(&redirect),
        enc("openid profile email offline_access"),
    );
    let wait = Box::pin(async move {
        let fut = async {
            loop {
                let (mut s, _) = listener.accept().await?;
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let target = head.split_whitespace().nth(1).unwrap_or("").to_string();
                let Some(q) = target.strip_prefix("/auth/callback?") else {
                    let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n").await;
                    continue;
                };
                let get = |k: &str| {
                    q.split('&')
                        .find_map(|p| p.strip_prefix(&format!("{k}=")))
                        .map(|v| v.replace("%2F", "/").replace("%3D", "="))
                };
                let page = |msg: &str| {
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{msg}",
                        msg.len()
                    )
                };
                if get("state").as_deref() != Some(state.as_str()) {
                    let _ =
                        s.write_all(page("<p>Login failed: the state didn't match. Try again.</p>").as_bytes()).await;
                    bail!("the login came back with a different state");
                }
                let Some(code) = get("code") else {
                    let _ = s.write_all(page("<p>Login failed.</p>").as_bytes()).await;
                    bail!("OpenAI sent no code: {}", get("error").unwrap_or_default());
                };
                let tokens = token_request(&[
                    ("grant_type", "authorization_code"),
                    ("code", &code),
                    ("redirect_uri", &redirect),
                    ("client_id", CLIENT_ID),
                    ("code_verifier", &verifier),
                ])
                .await;
                let msg = if tokens.is_ok() {
                    "<p>codeit is logged in to ChatGPT. You can close this tab.</p>"
                } else {
                    "<p>Login failed.</p>"
                };
                let _ = s.write_all(page(msg).as_bytes()).await;
                return save(&tokens?);
            }
        };
        tokio::time::timeout(Duration::from_secs(10 * 60), fut).await.map_err(|_| anyhow!("the login timed out"))?
    });
    Ok(Flow { login: DeviceLogin { url, code: String::new() }, wait })
}

// ── requests ────────────────────────────────────────────────────────────────

pub async fn chat(req: ChatRequest, events: UnboundedSender<ChatEvent>) -> Result<()> {
    let (token, account) = access().await?;
    let limits = CopilotModel {
        id: req.model.clone(),
        name: req.model.clone(),
        endpoint: Endpoint::Responses,
        context: None,
        max_input: None,
        max_output: None,
        efforts: vec!["low".into(), "medium".into(), "high".into()],
        adaptive_thinking: false,
        max_thinking_budget: None,
        picker_enabled: true,
        vision: true,
    };
    let mut body = protocols::responses_body(&req, "openai", Some(&limits));
    if body.get("instructions").is_none() {
        body["instructions"] = json!("");
    }
    debug("chatgpt.request", format!("model={}", req.model));
    let mut r = crate::http()
        .post(ENDPOINT)
        .bearer_auth(&token)
        .header("originator", "codeit")
        .header("session-id", &req.session_id)
        .header("user-agent", user_agent())
        .header("accept", "text/event-stream")
        .json(&body);
    if let Some(a) = &account {
        r = r.header("ChatGPT-Account-Id", a);
    }
    let res = r.send().await?;
    let status = res.status();
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        debug("chatgpt.request.http-error", format!("{status} {}", crate::snippet(&text)));
        let hint = match status.as_u16() {
            401 | 403 => " The ChatGPT login was rejected; run `/login openai` again.",
            429 => " You've reached your ChatGPT plan's limit for now.",
            _ => "",
        };
        bail!("ChatGPT {status}: {}{hint}", crate::snippet(&text));
    }
    protocols::responses_stream(res, &events).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_models_and_reads_accounts() {
        assert!(allowed("gpt-5.5") && allowed("gpt-6") && allowed("gpt-5.6-codex") && !allowed("gpt-5.6"));
        assert!(!allowed("gpt-4.1") && !allowed("gpt-5.4-pro") && !allowed("o3"));
        let claims = b64url(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acc_1"}}"#);
        assert_eq!(account_id(&format!("h.{claims}.s")).as_deref(), Some("acc_1"));
        assert_eq!(b64url_decode(&b64url(b"hello world")), b"hello world");
    }
}
