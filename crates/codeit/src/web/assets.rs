//! The web app's files, built from `web/` (`bun run build` there) into `crates/codeit/web-dist/`
//! and kept in the binary.

use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use include_dir::{Dir, include_dir};

static DIST: Dir = include_dir!("$CARGO_MANIFEST_DIR/web-dist");

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "json" => "application/json",
        "woff2" => "font/woff2",
        "js.map" | "map" => "application/json",
        _ => "application/octet-stream",
    }
}

pub async fn serve(req: Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');
    let (file, path) = match DIST.get_file(path) {
        Some(f) if !path.is_empty() => (f, path),
        // Everything else is the app (it reads the session from the URL).
        _ => match DIST.get_file("index.html") {
            Some(f) => (f, "index.html"),
            None => {
                return (StatusCode::NOT_FOUND, "the web app isn't built: run `bun install && bun run build` in web/")
                    .into_response();
            }
        },
    };
    // Built files have a hash in their name and never change; the page itself always might.
    let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    ([(header::CONTENT_TYPE, content_type(path)), (header::CACHE_CONTROL, cache)], file.contents()).into_response()
}
