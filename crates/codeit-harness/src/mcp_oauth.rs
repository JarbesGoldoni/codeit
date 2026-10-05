//! OAuth for remote MCP servers that require it (the MCP authorization spec): discovery of the
//! authorization server, dynamic client registration, the authorization code flow with PKCE
//! through the browser and a local callback, and refresh. Tokens are kept per server URL in
//! `~/.local/share/codeit/mcp-auth.json` (mode 600).

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The local callback, fixed so the registered redirect URI stays valid.
const CALLBACK_PORT: u16 = 19876;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Tokens {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    pub token_endpoint: String,
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Unix seconds.
    #[serde(default)]
    pub expires_at: Option<u64>,
}

fn store() -> std::path::PathBuf {
    codeit_providers::data_dir().join("mcp-auth.json")
}

fn load_all() -> BTreeMap<String, Tokens> {
    std::fs::read_to_string(store()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save(url: &str, tokens: Option<&Tokens>) -> Result<()> {
    let mut all = load_all();
    match tokens {
        Some(t) => all.insert(url.to_string(), t.clone()),
        None => all.remove(url),
    };
    let path = store();
    std::fs::create_dir_all(path.parent().expect("has a parent"))?;
    std::fs::write(&path, serde_json::to_string_pretty(&all)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn forget(url: &str) -> Result<()> {
    save(url, None)
}

pub fn has_tokens(url: &str) -> bool {
    load_all().contains_key(url)
}

fn b64url(bytes: &[u8]) -> String {
    crate::util::base64(bytes).trim_end_matches('=').replace('+', "-").replace('/', "_")
}

fn random() -> String {
    let mut bytes = Vec::new();
    for _ in 0..3 {
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    b64url(&bytes)
}

/// PKCE: (verifier, S256 challenge).
pub fn pkce() -> (String, String) {
    let verifier = random();
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Percent-decoding of a query value (`+` is a space).
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 3 <= b.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(v) => {
                    out.push(v);
                    i += 3;
                    continue;
                }
                Err(_) => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect::<Vec<_>>().join("&")
}

fn now() -> u64 {
    crate::util::now()
}

/// A token for the server: the stored one, refreshed when it expires within a minute.
/// `None` when there is none (or refreshing failed): the server needs `/mcp login`.
pub async fn token(url: &str) -> Option<String> {
    let t = load_all().get(url).cloned()?;
    if t.expires_at.is_none_or(|e| e > now() + 60) {
        return Some(t.access_token);
    }
    refresh(url).await.ok()
}

/// Exchanges the refresh token for a new access token.
pub async fn refresh(url: &str) -> Result<String> {
    let t = load_all().get(url).cloned().ok_or_else(|| anyhow!("not logged in"))?;
    let rt = t.refresh_token.clone().ok_or_else(|| anyhow!("the login expired and can't be refreshed"))?;
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", rt.as_str()),
        ("client_id", t.client_id.as_str()),
        ("resource", url),
    ];
    if let Some(s) = &t.client_secret {
        form.push(("client_secret", s));
    }
    let v = token_request(&t.token_endpoint, &form).await?;
    let fresh = Tokens {
        access_token: v["access_token"].as_str().ok_or_else(|| anyhow!("no access_token in the refresh"))?.into(),
        refresh_token: v["refresh_token"].as_str().map(String::from).or(t.refresh_token.clone()),
        expires_at: v["expires_in"].as_u64().map(|s| now() + s),
        ..t
    };
    save(url, Some(&fresh))?;
    Ok(fresh.access_token)
}

async fn token_request(endpoint: &str, form: &[(&str, &str)]) -> Result<Value> {
    let res = reqwest::Client::new()
        .post(endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .body(query(form))
        .timeout(Duration::from_secs(30))
        .send()
        .await?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("token endpoint answered {status}: {}", text.chars().take(200).collect::<String>());
    }
    Ok(serde_json::from_str(&text)?)
}

async fn get_json(url: &str) -> Result<Value> {
    let res = reqwest::Client::new()
        .get(url)
        .header("accept", "application/json")
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    if !res.status().is_success() {
        bail!("{url}: HTTP {}", res.status());
    }
    Ok(res.json().await?)
}

fn origin(url: &str) -> Result<String> {
    let rest = url.split_once("://").map(|x| x.1).ok_or_else(|| anyhow!("bad URL {url}"))?;
    let host = rest.split('/').next().unwrap_or(rest);
    Ok(format!("{}://{host}", &url[..url.find("://").unwrap_or(0)]))
}

/// The authorization server's metadata, found from the MCP server's protected resource
/// metadata (or, for older servers, from the MCP server's own origin).
pub async fn discover(url: &str, resource_metadata: Option<&str>) -> Result<Value> {
    let o = origin(url)?;
    let path = url[o.len()..].trim_end_matches('/');
    let mut candidates: Vec<String> = resource_metadata.map(|r| vec![r.to_string()]).unwrap_or_default();
    candidates.push(format!("{o}/.well-known/oauth-protected-resource{path}"));
    candidates.push(format!("{o}/.well-known/oauth-protected-resource"));
    let mut issuer = None;
    for c in candidates {
        if let Ok(v) = get_json(&c).await
            && let Some(a) = v["authorization_servers"].as_array().and_then(|a| a.first()).and_then(|a| a.as_str())
        {
            issuer = Some(a.trim_end_matches('/').to_string());
            break;
        }
    }
    let issuer = issuer.unwrap_or(o);
    let io = origin(&issuer)?;
    let ipath = issuer[io.len()..].to_string();
    for c in [
        format!("{io}/.well-known/oauth-authorization-server{ipath}"),
        format!("{io}/.well-known/openid-configuration{ipath}"),
        format!("{issuer}/.well-known/openid-configuration"),
    ] {
        if let Ok(v) = get_json(&c).await
            && v["authorization_endpoint"].is_string()
            && v["token_endpoint"].is_string()
        {
            return Ok(v);
        }
    }
    bail!("couldn't find how to log in to {url} (no OAuth metadata)")
}

/// The `resource_metadata` URL from a 401's `WWW-Authenticate` header.
pub fn resource_metadata(www_authenticate: &str) -> Option<String> {
    let i = www_authenticate.find("resource_metadata=")? + "resource_metadata=".len();
    let rest = &www_authenticate[i..];
    let v = rest.trim_start_matches('"');
    let end = v.find(['"', ',']).unwrap_or(v.len());
    Some(v[..end].to_string())
}

pub struct Login {
    /// The page to open in the browser.
    pub url: String,
    pending: tokio::task::JoinHandle<Result<()>>,
}

impl Login {
    /// Waits (up to 5 minutes) for the browser to come back with the code, and stores the tokens.
    pub async fn wait(self) -> Result<()> {
        match tokio::time::timeout(Duration::from_secs(300), self.pending).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => Err(anyhow!("the login stopped: {e}")),
            Err(_) => Err(anyhow!("no answer from the browser within 5 minutes")),
        }
    }
}

/// Starts logging in to the server at `url`: registers codeit as a client if needed, listens for
/// the callback, and returns the page to open.
pub async fn login(url: &str, resource_metadata: Option<&str>) -> Result<Login> {
    let meta = discover(url, resource_metadata).await?;
    let auth_endpoint = meta["authorization_endpoint"].as_str().expect("checked").to_string();
    let token_endpoint = meta["token_endpoint"].as_str().expect("checked").to_string();
    let redirect = format!("http://127.0.0.1:{CALLBACK_PORT}/callback");

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", CALLBACK_PORT))
        .await
        .with_context(|| format!("port {CALLBACK_PORT} is busy (another login running?)"))?;

    // Dynamic client registration, reusing an earlier registration for this server.
    let known = load_all().get(url).cloned();
    let (client_id, client_secret) = match known {
        Some(t) if t.token_endpoint == token_endpoint => (t.client_id, t.client_secret),
        _ => {
            let reg = meta["registration_endpoint"].as_str().ok_or_else(|| {
                anyhow!("{url} doesn't allow registering new clients; configure a token in its headers instead")
            })?;
            let res = reqwest::Client::new()
                .post(reg)
                .json(&json!({
                    "client_name": "codeit",
                    "redirect_uris": [redirect],
                    "grant_types": ["authorization_code", "refresh_token"],
                    "response_types": ["code"],
                    "token_endpoint_auth_method": "none",
                }))
                .timeout(Duration::from_secs(20))
                .send()
                .await?;
            if !res.status().is_success() {
                bail!("client registration failed: HTTP {}", res.status());
            }
            let v: Value = res.json().await?;
            (
                v["client_id"].as_str().ok_or_else(|| anyhow!("registration returned no client_id"))?.to_string(),
                v["client_secret"].as_str().map(String::from),
            )
        }
    };

    let (verifier, challenge) = pkce();
    let state = random();
    let mut params = vec![
        ("response_type", "code"),
        ("client_id", client_id.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", state.as_str()),
        ("resource", url),
    ];
    let scopes: Vec<String> = meta["scopes_supported"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(String::from))
        .collect();
    let scope = scopes.join(" ");
    if !scope.is_empty() {
        params.push(("scope", scope.as_str()));
    }
    let page = format!("{auth_endpoint}{}{}", if auth_endpoint.contains('?') { "&" } else { "?" }, query(&params));

    let url = url.to_string();
    let pending = tokio::spawn(async move {
        let code = loop {
            let (mut sock, _) = listener.accept().await?;
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await?;
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let target = req.split_whitespace().nth(1).unwrap_or("").to_string();
            let Some(q) = target.strip_prefix("/callback?") else {
                let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n").await;
                continue;
            };
            let get = |k: &str| q.split('&').find_map(|p| p.strip_prefix(&format!("{k}="))).map(decode);
            let ok = get("state").as_deref() == Some(state.as_str()) && get("code").is_some();
            let body = if ok {
                "<html><body style=\"font-family:sans-serif\"><h3>codeit is logged in.</h3>You can close this tab.</body></html>"
            } else {
                "<html><body style=\"font-family:sans-serif\"><h3>The login failed.</h3>Go back to codeit and try again.</body></html>"
            };
            let _ = sock
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            if !ok {
                bail!("the browser came back with {}", get("error").unwrap_or("an unexpected answer".into()));
            }
            break get("code").expect("checked");
        };
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("client_id", client_id.as_str()),
            ("code_verifier", verifier.as_str()),
            ("resource", url.as_str()),
        ];
        if let Some(s) = &client_secret {
            form.push(("client_secret", s));
        }
        let v = token_request(&token_endpoint, &form).await?;
        let tokens = Tokens {
            client_id: client_id.clone(),
            client_secret: client_secret.clone(),
            token_endpoint: token_endpoint.clone(),
            access_token: v["access_token"].as_str().ok_or_else(|| anyhow!("no access_token"))?.into(),
            refresh_token: v["refresh_token"].as_str().map(String::from),
            expires_at: v["expires_in"].as_u64().map(|s| now() + s),
        };
        save(&url, Some(&tokens))
    });
    Ok(Login { url: page, pending })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_the_s256_of_the_verifier() {
        // RFC 7636, appendix B.
        let challenge = b64url(&Sha256::digest(b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let (v, c) = pkce();
        assert!(v.len() >= 43 && !c.contains('='));
    }

    #[test]
    fn reads_the_resource_metadata_hint() {
        let h = r#"Bearer error="invalid_token", resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource""#;
        assert_eq!(
            resource_metadata(h).as_deref(),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource")
        );
        assert_eq!(origin("https://mcp.example.com/v1/sse").unwrap(), "https://mcp.example.com");
        assert_eq!(query(&[("a", "x y"), ("b", "c/d")]), "a=x%20y&b=c%2Fd");
        assert_eq!(decode("a%2Fb+c%"), "a/b c%");
    }
}
