# Notes for agents

- Read `README.md` first for the layout, build and test steps.
- `_base/`, when you have it, is local reference code to port from (opencode, Codex), ignored by git. Never build, edit or commit it.
- Crates: `codeit-providers` (talks to the APIs), `codeit-harness` (agent loop, tools, sessions; knows nothing about the TUI) `codeit` (TUI and CLI). The harness reports everything through `event::Event`, and extensions talk to the interface only through panels (`extension::Reply`); keep interface code out of both.
- To see the TUI without an account: `CODEIT_DEMO=1` adds a scripted `demo` provider (see README, Development).
- Prompts and tool descriptions are sent with every request: keep them short, and edit them in `crates/codeit-harness/src/prompts/` and each tool's `spec`.
- Test the loop with `crates/codeit-harness/tests/agent_loop.rs` (scripted provider). The real services can't be reached from the build environment; say so when something was only run against mocks.
- Before pushing: `cargo fmt --all`, `cargo clippy --all-targets`, `cargo test`.
