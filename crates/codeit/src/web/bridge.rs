//! The browser bridge: codeit's browser extension (`browser-extension/` in the repo) connects
//! here, gets the sites plugins asked for ([`Extension::sites`]), and sends their cookies
//! whenever they change. They go to the plugin ([`Extension::cookies`]), which says whether the
//! login works; when it doesn't, the extension is asked to reload the site, which often renews
//! it. The extension also loads each site's page now and then, so logins don't expire.
//!
//! Cookie values pass through here to the plugin only: they are never shown, logged or kept.
//!
//! [`Extension::sites`]: codeit_harness::extension::Extension::sites
//! [`Extension::cookies`]: codeit_harness::extension::Extension::cookies

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use codeit_harness::extension::{Cookie, Site};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::Server;

/// Reloads asked for because a login failed, at most this often per site.
const RELOAD_EVERY: Duration = Duration::from_secs(600);

struct Entry {
    /// `plugin:site`, unique across plugins.
    key: String,
    plugin: String,
    /// Index of the plugin in the harness's extensions.
    ext: usize,
    site: Site,
    /// (works, detail, when, in ms since the epoch) after the last cookies.
    status: Option<(bool, String, u64)>,
    /// The cookies last handed to the plugin, so the same ones aren't sent twice.
    last: Option<u64>,
    reloaded: Option<Instant>,
}

#[derive(Default)]
pub struct Bridge {
    sites: Mutex<Vec<Entry>>,
    clients: Mutex<HashMap<u64, (mpsc::UnboundedSender<String>, String)>>,
    next: AtomicU64,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum In {
    Hello { browser: Option<String> },
    Cookies { site: String, cookies: Vec<WireCookie> },
    Ping,
}

#[derive(Deserialize)]
struct WireCookie {
    name: String,
    value: String,
    #[serde(default)]
    domain: String,
}

fn fingerprint(cookies: &[Cookie]) -> u64 {
    let mut sorted: Vec<&Cookie> = cookies.iter().collect();
    sorted.sort_by(|a, b| (&a.name, &a.domain).cmp(&(&b.name, &b.domain)));
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for c in sorted {
        (&c.name, &c.value, &c.domain).hash(&mut h);
    }
    h.finish()
}

impl Bridge {
    /// What the app shows: the browsers connected and each site's state.
    pub fn describe(&self) -> Value {
        let clients = self.clients.lock().unwrap();
        let sites = self.sites.lock().unwrap();
        json!({
            "browsers": clients.values().map(|(_, b)| b.clone()).collect::<Vec<_>>(),
            "sites": sites.iter().map(|e| json!({
                "key": e.key,
                "plugin": e.plugin,
                "title": e.site.title,
                "url": e.site.url,
                "cookies": e.site.cookies,
                "keepalive": e.site.keepalive,
                "ok": e.status.as_ref().map(|s| s.0),
                "detail": e.status.as_ref().map(|s| s.1.clone()),
                "at": e.status.as_ref().map(|s| s.2),
            })).collect::<Vec<_>>(),
        })
    }

    fn sites_message(&self) -> String {
        let sites = self.sites.lock().unwrap();
        json!({
            "type": "sites",
            "sites": sites.iter().map(|e| json!({
                "key": e.key,
                "title": e.site.title,
                "url": e.site.url,
                "cookies": e.site.cookies,
                "keepalive": e.site.keepalive,
            })).collect::<Vec<_>>(),
        })
        .to_string()
    }

    fn to_browsers(&self, msg: &str) {
        for (tx, _) in self.clients.lock().unwrap().values() {
            let _ = tx.send(msg.to_string());
        }
    }

    /// Asks the browsers to reload a site. Unless `now`, at most every [`RELOAD_EVERY`].
    pub fn refresh(&self, key: &str, now: bool) {
        {
            let mut sites = self.sites.lock().unwrap();
            let Some(e) = sites.iter_mut().find(|e| e.key == key) else { return };
            if !now && e.reloaded.is_some_and(|t| t.elapsed() < RELOAD_EVERY) {
                return;
            }
            e.reloaded = Some(Instant::now());
        }
        self.to_browsers(&json!({"type": "refresh", "site": key}).to_string());
    }
}

fn changed(server: &Server) {
    server.broadcast(json!({"type": "bridge", "bridge": server.bridge.describe()}));
}

/// Asks every plugin for its sites (keeping what is known about the ones it had already).
pub async fn load_sites(server: &Arc<Server>) {
    let mut fresh = Vec::new();
    for (i, e) in server.harness.extensions.iter().enumerate() {
        for site in e.sites().await {
            fresh.push(Entry {
                key: format!("{}:{}", e.name(), site.id),
                plugin: e.name().to_string(),
                ext: i,
                site,
                status: None,
                last: None,
                reloaded: None,
            });
        }
    }
    {
        let mut sites = server.bridge.sites.lock().unwrap();
        for f in &mut fresh {
            if let Some(old) = sites.iter().find(|o| o.key == f.key) {
                f.status = old.status.clone();
                f.last = old.last;
                f.reloaded = old.reloaded;
            }
        }
        *sites = fresh;
    }
    server.bridge.to_browsers(&server.bridge.sites_message());
    changed(server);
}

/// One browser extension's connection.
pub async fn client(server: Arc<Server>, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let id = server.bridge.next.fetch_add(1, Ordering::Relaxed);
    server.bridge.clients.lock().unwrap().insert(id, (tx.clone(), "browser".into()));
    let _ = tx.send(server.bridge.sites_message());
    changed(&server);
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(Message::Text(m.into())).await.is_err() {
                break;
            }
        }
    });
    while let Some(Ok(msg)) = stream.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(m) = serde_json::from_str::<In>(&text) else { continue };
        match m {
            In::Hello { browser } => {
                if let Some((_, b)) = server.bridge.clients.lock().unwrap().get_mut(&id) {
                    *b = browser.unwrap_or_else(|| "browser".into());
                }
                changed(&server);
            }
            In::Ping => {
                let _ = tx.send(json!({"type": "pong"}).to_string());
            }
            In::Cookies { site, cookies } => {
                let (server, tx) = (server.clone(), tx.clone());
                tokio::spawn(async move { deliver(&server, &site, cookies, &tx).await });
            }
        }
    }
    writer.abort();
    server.bridge.clients.lock().unwrap().remove(&id);
    changed(&server);
}

/// Hands a site's cookies to its plugin, unless they are the ones it already has.
async fn deliver(server: &Arc<Server>, key: &str, cookies: Vec<WireCookie>, tx: &mpsc::UnboundedSender<String>) {
    let (ext, site_id, cookies) = {
        let mut sites = server.bridge.sites.lock().unwrap();
        let Some(e) = sites.iter_mut().find(|e| e.key == key) else { return };
        // Only the cookies the plugin asked for leave this function.
        let cookies: Vec<Cookie> = cookies
            .into_iter()
            .filter(|c| e.site.wants(&c.name))
            .map(|c| Cookie { name: c.name, value: c.value, domain: c.domain })
            .collect();
        let print = fingerprint(&cookies);
        if e.last == Some(print) && e.status.as_ref().is_some_and(|s| s.0) {
            return;
        }
        e.last = Some(print);
        (e.ext, e.site.id.clone(), cookies)
    };
    if cookies.is_empty() {
        return;
    }
    let Some(plugin) = server.harness.extensions.get(ext).cloned() else { return };
    let status = plugin.cookies(&site_id, cookies).await;
    {
        let mut sites = server.bridge.sites.lock().unwrap();
        if let Some(e) = sites.iter_mut().find(|e| e.key == key) {
            e.status = Some((status.ok, status.detail.clone(), super::now_ms()));
            if !status.ok {
                // Next time the same cookies come, try them again.
                e.last = None;
            }
        }
    }
    let _ = tx.send(json!({"type": "result", "site": key, "ok": status.ok, "detail": status.detail}).to_string());
    if !status.ok {
        server.bridge.refresh(key, false);
    }
    changed(server);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_cookies_in_any_order_match() {
        let a = Cookie { name: "a".into(), value: "1".into(), domain: "x".into() };
        let b = Cookie { name: "b".into(), value: "2".into(), domain: "x".into() };
        assert_eq!(fingerprint(&[a.clone(), b.clone()]), fingerprint(&[b.clone(), a.clone()]));
        let b2 = Cookie { value: "3".into(), ..b };
        assert_ne!(fingerprint(&[a.clone(), b2]), fingerprint(&[a]));
    }
}
