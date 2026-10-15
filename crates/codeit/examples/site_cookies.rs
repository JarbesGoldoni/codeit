//! A plugin that keeps a site's login from the browser: codeit with an extension that asks for
//! the `session_id` cookie of `SITE_URL` (codeit's browser extension sends it), and says the
//! login works unless the cookie is `expired`. Cookie values are never printed.
//!
//! ```sh
//! SITE_URL=https://app.example.com/ cargo run -p codeit --example site_cookies -- web
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use codeit_harness::extension::{Cookie, Extension, Site, SiteStatus};

struct SiteCookies {
    url: String,
}

#[async_trait]
impl Extension for SiteCookies {
    fn name(&self) -> &str {
        "example"
    }

    async fn sites(&self) -> Vec<Site> {
        vec![Site {
            id: "app".into(),
            title: "Example app".into(),
            url: self.url.clone(),
            cookies: vec!["session_id".into()],
            keepalive: Some(10),
        }]
    }

    async fn cookies(&self, site: &str, cookies: Vec<Cookie>) -> SiteStatus {
        let names: Vec<&str> = cookies.iter().map(|c| c.name.as_str()).collect();
        eprintln!("example: cookies for {site}: {}", names.join(", "));
        match cookies.iter().find(|c| c.name == "session_id") {
            Some(c) if c.value != "expired" => SiteStatus { ok: true, detail: "connected".into() },
            Some(_) => SiteStatus { ok: false, detail: "the session expired".into() },
            None => SiteStatus { ok: false, detail: "no session_id cookie".into() },
        }
    }
}

#[tokio::main]
async fn main() {
    let url = std::env::var("SITE_URL").unwrap_or_else(|_| "http://localhost:8000/".into());
    codeit::main(codeit::Plugins { extensions: vec![Arc::new(SiteCookies { url })], ..Default::default() }).await
}
