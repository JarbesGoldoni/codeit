//! GitHub Copilot login: GitHub's OAuth device flow, the same one opencode uses.
//! The GitHub OAuth token is sent to the Copilot API as is (no token exchange).
//!
//! The token is saved in `~/.local/share/codeit/auth.json` in opencode's format. If codeit has no
//! login yet, an existing opencode Copilot login (`~/.local/share/opencode/auth.json`) is used.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::DeviceLogin;

/// opencode's GitHub OAuth app. Override with CODEIT_COPILOT_CLIENT_ID.
const CLIENT_ID: &str = "Ov23li8tweQw6odWQebz";
const KEY: &str = "github-copilot";
/// Extra wait on every poll, so clock skew never makes us poll too early.
const POLL_MARGIN: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
pub struct Token {
    pub token: String,
    /// GitHub Enterprise domain, e.g. `company.ghe.com`.
    pub enterprise: Option<String>,
    /// Where it was read from, for status messages.
    pub source: String,
}

fn client_id() -> String {
    std::env::var("CODEIT_COPILOT_CLIENT_ID").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| CLIENT_ID.into())
}

pub fn codeit_auth_path() -> PathBuf {
    crate::data_dir().join("auth.json")
}

fn opencode_auth_path() -> PathBuf {
    crate::paths::xdg("XDG_DATA_HOME", ".local/share").join("opencode").join("auth.json")
}

fn read_entry(path: &PathBuf) -> Option<Token> {
    let text = std::fs::read_to_string(path).ok()?;
    let all: Value = serde_json::from_str(&text).ok()?;
    let e = &all[KEY];
    if e["type"] != "oauth" {
        return None;
    }
    let token = e["refresh"].as_str().or(e["access"].as_str()).filter(|t| !t.is_empty())?;
    Some(Token {
        token: token.into(),
        enterprise: e["enterpriseUrl"].as_str().map(String::from),
        source: path.display().to_string(),
    })
}

/// CODEIT_COPILOT_TOKEN, then codeit's own login, then opencode's.
pub fn load() -> Option<Token> {
    if let Ok(t) = std::env::var("CODEIT_COPILOT_TOKEN")
        && !t.is_empty()
    {
        return Some(Token { token: t, enterprise: None, source: "CODEIT_COPILOT_TOKEN".into() });
    }
    read_entry(&codeit_auth_path()).or_else(|| read_entry(&opencode_auth_path()))
}

pub fn save(token: &str, enterprise: Option<&str>) -> Result<()> {
    let path = codeit_auth_path();
    let mut all: Value =
        std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    let mut entry = json!({ "type": "oauth", "refresh": token, "access": token, "expires": 0 });
    if let Some(d) = enterprise {
        entry["enterpriseUrl"] = json!(d);
    }
    all[KEY] = entry;
    write_private(&path, &serde_json::to_string_pretty(&all)?)
}

pub fn remove() -> Result<bool> {
    let path = codeit_auth_path();
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(false) };
    let mut all: Value = serde_json::from_str(&text).unwrap_or(json!({}));
    let removed = all.as_object_mut().and_then(|o| o.remove(KEY)).is_some();
    write_private(&path, &serde_json::to_string_pretty(&all)?)?;
    Ok(removed)
}

fn write_private(path: &PathBuf, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write;
    let mut f = opts.open(path).with_context(|| format!("writing {}", path.display()))?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

pub fn normalize_domain(url: &str) -> String {
    url.trim().trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/').to_string()
}

/// A started device login: show `login` to the user, then call [`DeviceFlow::wait`].
pub struct DeviceFlow {
    pub login: DeviceLogin,
    domain: String,
    enterprise: Option<String>,
    device_code: String,
    interval: u64,
}

fn user_agent() -> String {
    format!("codeit/{}", env!("CARGO_PKG_VERSION"))
}

/// Starts the device flow on github.com, or on a GitHub Enterprise domain.
pub async fn start(enterprise: Option<&str>) -> Result<DeviceFlow> {
    let enterprise = enterprise.map(normalize_domain).filter(|d| !d.is_empty());
    let domain = enterprise.clone().unwrap_or_else(|| "github.com".into());
    let res = crate::http()
        .post(format!("https://{domain}/login/device/code"))
        .header("accept", "application/json")
        .header("user-agent", user_agent())
        .timeout(Duration::from_secs(20))
        .json(&json!({ "client_id": client_id(), "scope": "read:user" }))
        .send()
        .await?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("GitHub device login failed to start: {status} {}", crate::snippet(&text));
    }
    let v: Value = serde_json::from_str(&text)?;
    let get = |k: &str| v[k].as_str().map(String::from).ok_or_else(|| anyhow!("device login: missing {k}"));
    Ok(DeviceFlow {
        login: DeviceLogin { url: get("verification_uri")?, code: get("user_code")? },
        device_code: get("device_code")?,
        interval: v["interval"].as_u64().unwrap_or(5),
        domain,
        enterprise,
    })
}

impl DeviceFlow {
    /// Polls until the user approves (or denies) the login, then saves the token.
    pub async fn wait(self) -> Result<()> {
        let mut interval = self.interval;
        loop {
            tokio::time::sleep(Duration::from_secs(interval) + POLL_MARGIN).await;
            let res = crate::http()
                .post(format!("https://{}/login/oauth/access_token", self.domain))
                .header("accept", "application/json")
                .header("user-agent", user_agent())
                .timeout(Duration::from_secs(20))
                .json(&json!({
                    "client_id": client_id(),
                    "device_code": self.device_code,
                    "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                }))
                .send()
                .await?;
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            if !status.is_success() {
                bail!("GitHub login failed: {status} {}", crate::snippet(&text));
            }
            let v: Value = serde_json::from_str(&text)?;
            if let Some(token) = v["access_token"].as_str() {
                save(token, self.enterprise.as_deref())?;
                return Ok(());
            }
            match v["error"].as_str() {
                Some("authorization_pending") | None => {}
                // RFC 8628 §3.5: add 5 seconds, or use the interval GitHub sends.
                Some("slow_down") => interval = v["interval"].as_u64().filter(|i| *i > 0).unwrap_or(interval + 5),
                Some("expired_token") => bail!("The code expired before it was entered. Run the login again."),
                Some("access_denied") => bail!("The login was denied on GitHub."),
                Some(other) => bail!("GitHub login failed: {other}"),
            }
        }
    }
}
