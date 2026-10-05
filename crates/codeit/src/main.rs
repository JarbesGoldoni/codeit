//! codeit: a terminal coding agent, laid out like the Codex CLI.
//!
//! Crates:
//! - `codeit-providers`: model providers behind one trait.
//! - `codeit-harness`: the agent loop, tools, permissions, sessions and context.
//! - `codeit` (this one): the TUI and the non-interactive commands.

mod cli;
mod export;
mod login;
mod state;
mod tui;

use std::sync::Arc;

use codeit_harness::extension::Extension;

/// Extensions built into codeit (none yet; see README, Extensions).
pub fn extensions() -> Vec<Arc<dyn Extension>> {
    Vec::new()
}

#[tokio::main]
async fn main() {
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
