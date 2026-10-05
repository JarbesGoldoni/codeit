//! Remote MCP servers against an in-process mock: OAuth (discovery, registration, PKCE,
//! callback, token) on a streamable HTTP server, and the older SSE transport.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use codeit_harness::config::McpConfig;
use codeit_harness::mcp::{Client, NeedsLogin};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Default)]
struct State {
    challenge: Option<String>,
    sse: HashMap<String, tokio::sync::mpsc::UnboundedSender<Value>>,
}

fn rpc(msg: &Value) -> Value {
    match msg["method"].as_str().unwrap_or("") {
        "initialize" => {
            json!({ "protocolVersion": "2025-06-18", "capabilities": { "tools": {} }, "serverInfo": { "name": "mock" } })
        }
        "tools/list" => {
            json!({ "tools": [{ "name": "echo", "description": "Echo", "inputSchema": { "type": "object" } }] })
        }
        "tools/call" => {
            json!({ "content": [{ "type": "text", "text": format!("echo: {}", msg["params"]["arguments"]["text"].as_str().unwrap_or("")) }] })
        }
        _ => json!({}),
    }
}

async fn reply(s: &mut TcpStream, status: &str, headers: &[(&str, String)], body: &str) {
    let mut out = format!("HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n", body.len());
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(body);
    let _ = s.write_all(out.as_bytes()).await;
}

fn query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| {
            let v = v.replace('+', " ");
            let mut out = Vec::new();
            let b = v.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' && i + 2 < b.len() {
                    out.push(u8::from_str_radix(&v[i + 1..i + 3], 16).unwrap());
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            (k.to_string(), String::from_utf8(out).unwrap())
        })
        .collect()
}

async fn serve(mut s: TcpStream, base: String, state: Arc<Mutex<State>>) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let (head, body) = loop {
        let n = s.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&tmp[..n]);
        let text = String::from_utf8_lossy(&buf).to_string();
        if let Some(end) = text.find("\r\n\r\n") {
            let head = text[..end].to_string();
            let len: usize = head
                .lines()
                .find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
                .unwrap_or(0);
            if buf.len() >= end + 4 + len {
                break (head, String::from_utf8_lossy(&buf[end + 4..end + 4 + len]).to_string());
            }
        }
    };
    let first = head.lines().next().unwrap_or("").to_string();
    let mut parts = first.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let (path, q) = target.split_once('?').unwrap_or((target, ""));
    let q = query(q);
    let auth = head
        .lines()
        .find_map(|l| l.strip_prefix("authorization: ").or(l.strip_prefix("Authorization: ")))
        .unwrap_or("")
        .to_string();
    let json_ct = [("content-type", "application/json".to_string())];
    match (method, path) {
        ("GET", "/.well-known/oauth-protected-resource/mcp") => {
            reply(
                &mut s,
                "200 OK",
                &json_ct,
                &json!({ "resource": format!("{base}/mcp"), "authorization_servers": [base] }).to_string(),
            )
            .await
        }
        ("GET", "/.well-known/oauth-authorization-server") => {
            let m = json!({ "issuer": base, "authorization_endpoint": format!("{base}/authorize"), "token_endpoint": format!("{base}/token"), "registration_endpoint": format!("{base}/register") });
            reply(&mut s, "200 OK", &json_ct, &m.to_string()).await
        }
        ("POST", "/register") => {
            let v: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(v["token_endpoint_auth_method"], "none");
            reply(&mut s, "201 Created", &json_ct, &json!({ "client_id": "cid" }).to_string()).await
        }
        ("GET", "/authorize") => {
            assert_eq!(q["code_challenge_method"], "S256");
            assert_eq!(q["resource"], format!("{base}/mcp"));
            assert_eq!(q["client_id"], "cid");
            state.lock().unwrap().challenge = Some(q["code_challenge"].clone());
            let loc = format!("{}?code=code123&state={}", q["redirect_uri"], q["state"]);
            reply(&mut s, "302 Found", &[("location", loc)], "").await
        }
        ("POST", "/token") => {
            let f = query(&body);
            if f["grant_type"] == "authorization_code" {
                let digest = Sha256::digest(f["code_verifier"].as_bytes());
                let got =
                    codeit_harness::util::base64(&digest).trim_end_matches('=').replace('+', "-").replace('/', "_");
                assert_eq!(Some(got), state.lock().unwrap().challenge.clone(), "PKCE verifier matches the challenge");
                assert_eq!(f["code"], "code123");
            }
            reply(
                &mut s,
                "200 OK",
                &json_ct,
                &json!({ "access_token": "tok1", "refresh_token": "r1", "expires_in": 3600 }).to_string(),
            )
            .await
        }
        ("POST", "/mcp") => {
            if auth != "Bearer tok1" {
                let www = format!("Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\"");
                return reply(&mut s, "401 Unauthorized", &[("www-authenticate", www)], "").await;
            }
            let msg: Value = serde_json::from_str(&body).unwrap();
            if msg.get("id").is_none() {
                return reply(&mut s, "202 Accepted", &[], "").await;
            }
            reply(
                &mut s,
                "200 OK",
                &json_ct,
                &json!({ "jsonrpc": "2.0", "id": msg["id"], "result": rpc(&msg) }).to_string(),
            )
            .await
        }
        ("GET", "/sse") => {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let sid = format!("s{}", state.lock().unwrap().sse.len() + 1);
            state.lock().unwrap().sse.insert(sid.clone(), tx);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n").await;
            let _ = s.write_all(format!("event: endpoint\ndata: /messages?sid={sid}\n\n").as_bytes()).await;
            while let Some(msg) = rx.recv().await {
                if s.write_all(format!("event: message\ndata: {msg}\n\n").as_bytes()).await.is_err() {
                    break;
                }
            }
        }
        ("POST", "/messages") => {
            let msg: Value = serde_json::from_str(&body).unwrap();
            reply(&mut s, "202 Accepted", &[], "").await;
            if msg.get("id").is_some() {
                let tx = state.lock().unwrap().sse.get(&q["sid"]).cloned().unwrap();
                let _ = tx.send(json!({ "jsonrpc": "2.0", "id": msg["id"], "result": rpc(&msg) }));
            }
        }
        _ => reply(&mut s, "404 Not Found", &[], "").await,
    }
}

async fn mock() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let state: Arc<Mutex<State>> = Arc::default();
    let b = base.clone();
    tokio::spawn(async move {
        loop {
            let Ok((s, _)) = listener.accept().await else { break };
            tokio::spawn(serve(s, b.clone(), state.clone()));
        }
    });
    base
}

fn remote(url: &str) -> McpConfig {
    McpConfig::Remote { url: url.into(), headers: BTreeMap::new(), enabled: true, timeout: Some(10_000) }
}

#[tokio::test]
async fn logs_in_with_oauth_and_speaks_both_transports() {
    let home = std::env::temp_dir().join(format!("codeit-mcp-{}", uuid::Uuid::new_v4()));
    unsafe {
        std::env::set_var("XDG_DATA_HOME", &home);
        std::env::set_var("XDG_CACHE_HOME", home.join("cache"));
    }
    let base = mock().await;
    let cwd = std::env::temp_dir();

    // Not logged in: the server asks for a login, with its metadata hint.
    let url = format!("{base}/mcp");
    let err = Client::connect("secure", &remote(&url), &cwd).await.err().expect("needs a login");
    let hint = err.downcast_ref::<NeedsLogin>().expect("NeedsLogin").0.clone();
    assert_eq!(hint, Some(format!("{base}/.well-known/oauth-protected-resource/mcp")));

    // The login: codeit registers, the "browser" follows the authorize redirect to the callback.
    let login = codeit_harness::mcp_oauth::login(&url, hint.as_deref()).await.unwrap();
    let browser = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let res = browser.get(&login.url).send().await.unwrap();
    let callback = res.headers()["location"].to_str().unwrap().to_string();
    assert!(callback.starts_with("http://127.0.0.1:19876/callback?code=code123"));
    let page = browser.get(&callback).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("codeit is logged in"));
    login.wait().await.unwrap();
    let stored = std::fs::read_to_string(home.join("codeit/mcp-auth.json")).unwrap();
    assert!(stored.contains("\"client_id\": \"cid\""));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.join("codeit/mcp-auth.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // Logged in: tools load and calls work.
    let c = Client::connect("secure", &remote(&url), &cwd).await.unwrap();
    assert_eq!(c.tools[0].name, "echo");
    let (text, _, error) = c.call("echo", json!({ "text": "hi" })).await.unwrap();
    assert_eq!((text.as_str(), error), ("echo: hi", false));

    // The older SSE transport.
    let legacy = Client::connect("legacy", &remote(&format!("{base}/sse")), &cwd).await.unwrap();
    let (text, _, _) = legacy.call("echo", json!({ "text": "old" })).await.unwrap();
    assert_eq!(text, "echo: old");
    let _ = std::fs::remove_dir_all(&home);
}
