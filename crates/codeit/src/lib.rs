//! codeit: a terminal coding agent, laid out like the Codex CLI.
//!
//! Crates:
//! - `codeit-providers`: model providers behind one trait.
//! - `codeit-harness`: the agent loop, tools, permissions, sessions and context.
//! - `codeit` (this one): the TUI and the non-interactive commands.
//!
//! Plugins live in their own crates and build their own binary: a `main` that calls [`main`]
//! with the providers and extensions they add (see README, Plugins).

mod cli;
mod export;
mod login;
mod state;
mod tui;

use std::sync::{Arc, OnceLock};

use codeit_harness::extension::Extension;
use codeit_providers::Provider;

/// What a plugin binary adds to codeit.
#[derive(Default)]
pub struct Plugins {
    pub providers: Vec<Arc<dyn Provider>>,
    pub extensions: Vec<Arc<dyn Extension>>,
}

static PLUGINS: OnceLock<Plugins> = OnceLock::new();

/// The providers codeit ships with, then the plugins'.
pub(crate) fn providers() -> Vec<Arc<dyn Provider>> {
    let mut v = codeit_providers::all();
    if let Some(p) = PLUGINS.get() {
        v.extend(p.providers.iter().cloned());
    }
    v
}

pub(crate) fn extensions() -> Vec<Arc<dyn Extension>> {
    PLUGINS.get().map(|p| p.extensions.clone()).unwrap_or_default()
}

/// Runs codeit with `plugins`, then exits the process.
pub async fn main(plugins: Plugins) {
    let _ = PLUGINS.set(plugins);
    codeit_providers::migrate_from_done();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = cli::run(args).await;
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
    // The terminal reader thread may still be blocked on input.
    std::process::exit(0);
}
