# codeit

The open source AI coding agent for your terminal, written in Rust.

codeit works like [opencode](https://github.com/anomalyco/opencode): the same capabilities, the same config shapes, and the same providers. It logs in to GitHub Copilot, OpenAI (ChatGPT Plus/Pro or an API key), Anthropic, Google, OpenRouter and the rest of the models.dev catalog, local servers like Ollama, plus Z.ai's GLM Coding Plan. It even uses your opencode logins.

What it adds:

- **A calm, readable UI.** On a soft gray background: your messages and the answers in bubbles, thinking in gray (both shown smoothly as they stream), actions in a faint frame, a side bubble with the context used and your branch, and popups for models, sessions and effort.
- **Light.** One native binary of about 18 MB, with no Node or Bun runtime behind it.

codeit reads, searches and edits code, runs commands, plans, delegates to subagents, asks before risky actions, and keeps sessions you can resume and undo. It reviews diffs like a pull request (`/review`) and uses a cheaper model to condense long tool output (`/models`).

It runs on macOS, Linux and Windows (through WSL).

## Install

You need a Rust toolchain ([rustup.rs](https://rustup.rs)) and a C compiler (the TLS library builds some C): Xcode's command line tools on macOS, `build-essential` or your distribution's equivalent on Linux and WSL.

```bash
cargo install --git https://github.com/JarbesGoldoni/codeit codeit
```

Or from a clone:

```bash
cargo build --release
./target/release/codeit --help
cargo install --path crates/codeit   # puts codeit on your PATH (~/.cargo/bin)
```

## Log in

```bash
codeit login            # asks which provider and how
codeit login anthropic  # or name it
codeit providers        # every provider you can log in to (● logged in)
codeit status           # the ones you're logged in to
```

The providers are opencode's: the models.dev catalog (OpenAI, Anthropic, Google, OpenRouter, xAI, Groq, DeepSeek, Mistral, Moonshot, opencode Zen and about two hundred more), cached in `~/.cache/codeit/models.json` and refreshed daily. Most log in with an API key, typed without echo and saved in `~/.local/share/codeit/auth.json` (readable only by you, opencode's format); each provider's usual environment variable works too (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`…). codeit also uses your opencode logins (`~/.local/share/opencode/auth.json`), so you may already be logged in. In the TUI, `/login` does the same in a popup.

- **GitHub Copilot**: a GitHub device login (github.com or GitHub Enterprise): open the URL, enter the code, approve.
- **OpenAI**: a ChatGPT Plus/Pro login (in the browser, or with a code when there's no browser here), or an API key.
- **Z.ai GLM Coding Plan** (`zai-coding-plan`): the coding plan's key (or `ZAI_API_KEY`). Effort is GLM's thinking switch: `on` (default) or `off`. Z.ai's pay-as-you-go API is the catalog's `zai`.
- **Local servers** (Ollama, LM Studio and the other local ones in the catalog): no login; their loaded models show while they run.

`codeit logout <provider>` removes a login codeit saved.
## Use it

```bash
codeit                           # the TUI
codeit -c                        # the TUI, on the last session in this folder
codeit run "fix the failing test in src/parser.rs"     # one task, non-interactively
codeit run -m copilot/claude-sonnet-5 --agent plan "how would you add caching here?"
codeit sessions                  # sessions started in this folder
```

In the TUI, describe a task and codeit works on it: you see its thinking, its actions in a faint frame, and its answer in a bubble (see TUI). Press Esc to stop it. Type while it works to queue the next message.

- **Tab** switches between **build** (does the work) and **plan** (read-only: investigates and proposes a plan). Tab back to build to carry the plan out.
- **@path** in a message attaches that file (or folder listing); an image file (PNG, JPEG, GIF, WebP, up to 5 MB) is attached as an image.
- **Images**: ctrl+v, alt+v or `/paste` attaches the screenshot in the clipboard (on WSL, the Windows clipboard; if Windows Terminal keeps ctrl+v for itself, use alt+v or `/paste`). Pasting or dropping an image file's path attaches it too. `read` on an image gives the model the image. Models that can't see images get a note instead, and images older than the last two turns are dropped from the context.
- **/undo** removes the last turn and restores the files it changed. In a git project that includes what its shell commands changed (created, edited, deleted): codeit snapshots the files before and after each turn in a separate repository under `~/.local/share/codeit/snapshot/`, never touching yours, and `.gitignore` applies. A file you changed after the turn is left alone and named. Outside git, the edits codeit made itself are restored. **/session** resumes an earlier session. **/compact** summarizes the conversation to free context (codeit also does this on its own when the context is nearly full).
- **Customize codeit by asking.** "Make a /deploy command that..." or "add a skill for our API": the built-in `customize-codeit` skill tells the agent where commands, skills, agents and settings go. New commands and skills work after that turn, without a restart.
- **/review** opens the review screen (see Code review).
- **/approvals ask** makes codeit ask before every edit, command (other than read-only ones like `ls` or `git status`) and web fetch. The default, **auto**, is opencode's: it only asks before touching files outside the project, reading `.env` files, or repeating the same call three times.

`codeit run` prints the reply on stdout and the tool calls on stderr. Anything that would ask is refused unless you pass `--yes`; `-c` continues the last session.

## Test it, step by step

1. **Check logins and models:**
   ```bash
   codeit status
   codeit models
   ```
2. **One agent run, in a scratch repository** (so nothing you care about is touched):
   ```bash
   mkdir -p /tmp/codeit-test && cd /tmp/codeit-test && git init -q
   printf 'def add(a, b):\n    return a - b\n' > calc.py
   codeit run -m copilot/gpt-4.1 "calc.py has a bug in add; fix it and show me the diff with git"
   codeit run -m copilot/claude-sonnet-5 -c "now add a test file for add and run it with python3"
   ```
   You should see `→ read calc.py`, `→ edit calc.py`, `→ bash ...` lines, then the answer. Try one model from each Copilot family, since each uses a different API: a GPT-4 class model (`/chat/completions`), a GPT-5 class model (`/responses`, which edits with `apply_patch`), and a Claude model (`/v1/messages`).
3. **The TUI**, in the same folder: `codeit`. Pick a model with `/models`, ask for a change, watch the diff, then `/undo` it. Press Tab for plan mode and ask for a plan. Run `/approvals ask` and ask for an edit to see the permission prompt. Quit and come back with `codeit -c`.

If something fails, rerun it with `CODEIT_DEBUG=1` and open an issue with the error printed on screen and the relevant part of `~/.cache/codeit/debug.log`. The log never contains tokens. Add `CODEIT_DEBUG_BODY=1` to also log request bodies, which include your prompts and file contents.

## TUI

The screen has your conversation on the left and a side bubble on the right (when the terminal is at least 100 columns wide):

- **Your messages** are in blue bubbles, as wide as the input box.
- **The agent's thinking** is gray text, lined up with the text in the bubbles.
- **Its actions** (reads, searches, edits, commands, fetches) are in a faint frame, one row each: the tool, what it worked on, and the result on the right (`631 lines`, `+8 −3`, `14 passed`), with the time. A failure shows only through its result, in a soft red (`exit 101`). An action enters the frame when it finishes; while it runs, the status line under the conversation says what it is (`⠼ Editing calc.py (2s • esc to interrupt)`), and a command shows its last lines of output as they come.
- **Its answer** is in a gray bubble with the agent, model, effort and time in the corner (`build · GLM-5.1 · high · 38s`).
- **The side bubble** shows the session's title and how much context is used, the branch (with `+new ~changed` files) and the folder. The model's todo list sits at its bottom: ● in progress, ○ pending, ✔ done, ✗ cancelled. (In a narrow terminal it shows in the conversation instead.)

Actions show at one of three levels, Ctrl+O cycles them (remembered): **folded** (one line per group: `▸ 4 actions · read 2 · edited 1 · ran 1 · 1 failed`), **list** (one row per action, the default) and **open** (each action with its output trimmed: a failed command's error lines, an edit's first changes). ↑/↓ select an action (or a folded group) and Enter opens it in full, or closes it.

| Key | Action |
|---|---|
| Enter | send (queues it while codeit is working) |
| Shift+Enter, Alt+Enter, Ctrl+J | new line |
| ↑ / ↓ while typing a `/` command | move through the matching commands; Enter runs the highlighted one |
| Tab | complete the highlighted `/` command; otherwise switch agent (build ↔ plan) |
| Ctrl+T | cycle the reasoning effort (default → each level the model supports) |
| Esc | stop the current answer |
| ↑ / ↓ (empty input) | select an action or a folded group (↑ first takes back a queued message) |
| Enter on a selected action | show its whole output, or close it; on a folded group, unfold it |
| Esc Esc (empty input) | edit your last message; sending it rewinds the conversation (and the files) to before it |
| Ctrl+O | actions folded, as a list, or open (remembered) |
| PgUp / PgDn, mouse wheel | scroll the history (the wheel also moves through the review screen, panels and popups) |
| drag | select text; it is copied when you let go (frames left out). `"tui": {"mouse": false}` leaves the mouse to the terminal |
| Ctrl+C | stop the answer, then clear the input, then quit |

In a permission prompt: `y` yes, `a` always (for the rest of the session), `n` no and say what to do instead, Esc no.

`/models`, `/session` and `/login` open a popup in the middle, like a command palette: type to filter, ↑↓ to move, Enter to choose, Esc to close.

| Command | Action |
|---|---|
| `/models` | pick the model for this session |
| `/effort [level\|default]` | choose the reasoning effort in a popup, or set it directly |
| `/agent [name]` | switch agent |
| `/new` | start a new session; a turn still running keeps going in the background |
| `/session` | switch to another session of this folder, even while a turn runs: it keeps working in the background (the footer counts them, and the list marks them working or waiting for you); ctrl+r renames the selected one, ctrl+d (twice) deletes it |
| `/undo` | undo the last turn and the file changes it made (shell commands' too, in git projects) |
| `/compact` | summarize the conversation to free context |
| `/approvals [auto\|ask]` | ask before edits and commands, or not |
| `/rename <title>` | rename this session |
| `/copy` | copy codeit's last answer to the clipboard (pbcopy, clip.exe, wl-copy, xclip or xsel; otherwise the terminal's OSC 52, which works over SSH) |
| `/export [path]` | save this session as Markdown (default `~/.local/share/codeit/exports/`) |
| `/review [target]` | open the review screen on your uncommitted changes, a branch (`/review main`: this branch's changes since main), a commit, or a PR (`/review pr 12`, `#12` or its URL, through `gh`) |
| `/<your command> [args]` | run a custom prompt command (see Commands) |
| `/login [provider]` | log in to a provider (the popup lists them, ● logged in) |
| `/logout <provider>` | remove a login codeit saved |
| `/status` | logins, MCP servers, language servers, skills, instruction files, session |
| `/mcp [login\|logout\|reconnect name]` | a list of the MCP servers and their state (green on, red off, yellow connecting, bright red broken): space turns one on or off for this run; log in to one that uses OAuth |
| `/help` | list commands and keys |
| `/exit` | exit codeit |

## Code review

`/review` shows a diff like a pull request: the changed files on the right (with their comment counts), the selected file's diff in the middle, and comments under the lines they are about.

- **r** has codeit review it. The `review` agent runs on the session's model, reads the diff and the surrounding code, and leaves comments on lines with a severity (`bug`, `risk`, `question`, `nit`); it can't edit anything. Its overall assessment shows at the top. Comments can only go on lines in the diff, so they always line up.
- **a** agree, **d** dismiss, **o** reopen, **D** delete the comment under the cursor; **c** writes your own comment on the selected line.
- **f** sends the agreed comments to the build agent to fix, and goes back to the conversation.
- **e** exports the review as Markdown (agreed, then open comments), for example to send to a colleague.
- ↑↓ or j/k move, n/p (or ]/[) change file, Tab moves to the file list, J/K jump to the next or previous comment, u reloads the diff, q or Esc closes (Esc first stops a running review).

Reviews are saved per folder and target in `~/.local/share/codeit/reviews/`, so reopening one keeps its comments and your verdicts; comments whose line has left the diff show as outdated at the top of the file. For a colleague's PR, codeit reviews `gh pr diff` without checking it out.

## How the harness works

Each turn, codeit sends the conversation, the system prompt and the tool definitions to the model. The model answers with text and tool calls; codeit runs the calls (read-only ones in parallel), sends the results back, and repeats until the model answers without calling a tool. Sessions are saved after every step under `~/.local/share/codeit/sessions/`.

### Tools

| Tool | What it does |
|---|---|
| `read` | A file with line numbers (up to 2000 lines per call, `offset`/`limit` for more), an image (given to the model as an image), or a folder listing |
| `edit` | Exact replacement of `old_string` with `new_string`, one or several per call; used by every model except GPT-5 class |
| `write` | Create a file or replace all of it |
| `apply_patch` | Codex's patch format, which GPT-5 class models are trained on; replaces edit and write for them |
| `bash` | A shell command, with a timeout (2 min default, 10 max), in its own process group so Esc stops all of it |
| `grep`, `glob` | Content search (regex) and file search; built in, they respect `.gitignore` |
| `todo` | The model's plan, shown to you as it changes. The model is reminded when it goes a few steps without updating it, and asked once to close open items when its turn ends |
| `task` | Hands work to a subagent with its own context (`explore` for read-only searching, `general` for anything); several run in parallel; `task_id` continues one |
| `skill` | Loads a skill's instructions |
| `webfetch` | A URL as text |
| `lsp` | Code intelligence from the language server: definition, references, hover, document and workspace symbols, implementations, call hierarchy |
| `websearch` | Searches the web (Exa's public search, the one opencode uses; `EXA_API_KEY` raises its limits) and returns the most relevant pages' content |
| `question` | Asks you multiple-choice questions |
| MCP tools | Every tool of the MCP servers in the config, as `<server>_<tool>` |

Not ported from opencode: sharing sessions through opencode's hosted service; `/export` writes a session as Markdown instead.

### Language servers

After every edit, the language servers that handle the file report its errors back to the model with the edit's result, so it fixes them right away. The `lsp` tool gives it definitions, references, types and symbols. Built-in servers are used when their program is on your PATH (codeit installs nothing): `rust-analyzer`, `typescript-language-server`, `pyright-langserver`, `gopls`, `jdtls` (Java), `kotlin-language-server`, `clangd`. Each starts the first time a file it handles is touched, rooted at the nearest project file (`Cargo.toml`, `build.gradle`, `pom.xml`, `package.json`...). `/status` lists the installed and running ones. Add or change servers in the `lsp` config key, as in opencode:

```jsonc
"lsp": {
  "jdtls": { "command": ["/opt/jdtls/bin/jdtls", "-data", "/tmp/jdtls"] },
  "zig": { "command": ["zls"], "extensions": [".zig"] },
  "gopls": { "disabled": true }
}
```

`"lsp": false` turns them all off.

### Precision

- **Read before edit.** An existing file can only be edited or overwritten after the model has read it, and again if it changed on disk since. This stops edits made from a stale or imagined version of the file.
- **Forgiving matching.** When `old_string` doesn't match exactly, edit tries the near misses models make: indentation, trailing spaces, escaped characters, a slightly different middle of a block. When it matches with different indentation, the replacement is re-indented to fit. When nothing matches, the error shows the closest lines in the file, so the model can retry without reading the file again.
- **Edits confirm themselves.** A successful edit returns the changed lines with their numbers, so the model doesn't re-read the file to check.
- **Instructions where they apply.** AGENTS.md in a subfolder is attached the first time the model reads a file under it.
- **Loops are caught.** The same call with the same arguments three times in a row asks you before it runs.

### Fewer tokens

- A short system prompt (about 650 tokens, against 2–4k in opencode) and short tool descriptions. They don't change during a session, so providers can cache them.
- Command output is cleaned of colors and progress bars. Output over 400 lines or 24 KB keeps its start and end, and the full output is saved to a file the model can grep instead of rerunning the command.
- Paths in tool output are relative to the working directory. Search results are grouped by file.
- Reading a file range again when the file hasn't changed returns a one-line pointer to the earlier result.
- Old tool output is pruned at the end of a turn, and only when it frees at least 20k tokens. The last two turns and the newest 40k tokens of output are always kept, and so are loaded skills. Pruning in batches means the provider's prompt cache is invalidated rarely.
- **Long tool output is condensed.** A command, web fetch or MCP result over about 2k tokens goes to the small model first. The agent gets what ran and how it ended, the key results, every error line copied verbatim by codeit (not by the model, so nothing depends on its wording), and the path of the full output to grep. Reads and searches are never condensed. In the TUI the step is marked `condensed` and, unfolded, shows the digest after `≈`. If the helper fails or takes over 45s, the output goes in cut to its start and end as before.
- When the context nears the model's limit, the older part of the conversation is replaced by a structured summary (objective, decisions, done and pending work, relevant files), and the most recent turns are kept as they are.
- Session titles: the first message of a session is sent once to the small model (else the session's), in the background, to name it in a few words. If that fails, the title is the message's first line.
- On Copilot, only the requests you type count as premium requests: tool follow-ups, subagents, summaries and condensing are sent as `x-initiator: agent`, as opencode and VS Code do.
- Summaries and condensing use the **small** model (`small_model` in the config) when its context can hold the input, else the session's model.

### Agents

| Agent | |
|---|---|
| `build` | The default. Every tool. |
| `plan` | Read-only. Edits are refused (except Markdown plans under `.codeit/plans/`), and a reminder in each message tells the model not to change anything. |
| `general` | Subagent for delegated work. Every tool except todo and task. |
| `explore` | Subagent for searching: read, grep, glob, bash, webfetch, skill. |
| `review` | Runs from the review screen: read-only, plus `review_comment`. Not in the Tab cycle. |

Add your own in `codeit.json` (`agent`) or as Markdown files in `.codeit/agents/` or `~/.config/codeit/agents/` (frontmatter `description`, `mode`, `model`, `steps`; the body is the prompt), as in opencode. opencode's agent folders are read too.

### Permissions

Rules are the same as opencode's: a permission (a tool name, or `edit` for every file change, `external_directory`, `doom_loop`), a pattern (a path relative to the project root, a command, a URL), and `allow`, `ask` or `deny`. The last matching rule wins. They're applied in this order: the defaults, your `permission` config, the `/approvals ask` preset, then the agent's own rules. Answering "always" adds a rule for the rest of the session. For bash, each command in a pipeline or `&&` chain is checked on its own, and "always" covers that command's prefix (`git push`, `npm run`, `ls`).

### Context the model gets

- **Instructions.** `AGENTS.md` from the project root down to the working directory (or `CLAUDE.md` where there is none), a global `~/.config/codeit/AGENTS.md` (or opencode's, or `~/.claude/CLAUDE.md`), and the files, globs or URLs listed in `instructions`.
- **Skills.** `SKILL.md` files in `.codeit/skills/`, `~/.config/codeit/skills/`, opencode's, Claude's and `.agents` skill folders, and `skills.paths`. Each skill's name and description are listed in the system prompt, and the model loads the full skill when a task matches. Your existing opencode and Claude skills work as they are. One is built in, `customize-codeit` (a skill with the same name replaces it).
- **Commands.** Markdown prompt templates in `.codeit/commands/`, `~/.config/codeit/commands/`, opencode's and Claude's command folders, and the `command` config key. `$ARGUMENTS`, `$1`, `$2`… are replaced, `` !`cmd` `` becomes the command's output, frontmatter can set `description`, `agent`, `model` and `subtask` (run it in a subagent). Command and skill files are read again after each turn, so new ones work without a restart; config keys, agents and MCP servers are read at startup.
- **MCP servers.** Local (stdio) and remote servers from the `mcp` config key, in opencode's format. Remote servers speak streamable HTTP, or the older HTTP+SSE transport (used when the URL ends in `/sse`, or when a server refuses streamable HTTP). A remote server without an `Authorization` header that asks for OAuth shows as "needs a login" in `/mcp`; `/mcp login <name>` opens the browser, registers codeit with the server's authorization server, and stores the tokens (refreshed automatically) in `~/.local/share/codeit/mcp-auth.json`, mode 600. `/mcp logout <name>` forgets them. Images MCP tools return go to the model. Their tools are offered as `<server>_<tool>` and their instructions join the system prompt. They connect in the background; `/status` shows them.
- **The environment.** Working directory, git root, platform (WSL is detected), date and model.

### Plugins

Plugins live in their own repos and build their own binary: a Rust crate that depends on codeit's crates (by git) and calls `codeit::main(codeit::Plugins { providers, extensions })` from its `main`. That binary is codeit plus the plugin, with the same config, logins and sessions. A plugin provider implements `codeit_providers::Provider`; when it logs in with its own tool, `/login` shows what its status says to do. For example, a plugin can add a provider that uses another tool's login.

An extension is a Rust value implementing `codeit_harness::extension::Extension`. It can change the config at startup (add skill folders, instruction files, commands, MCP servers), add to the system prompt, set environment variables for every bash command, and add tools, like opencode plugins' `config`, `experimental.chat.system.transform` and `shell.env` hooks.

It can show a few words in the footer (a quota, say), refreshed at start, every 5 minutes and after a turn. It can also add slash commands that open a **panel**. The extension answers each action (opened, Enter on a row, one of its keys, text typed, a poll) with a reply: a panel (titled sections of rows, each with a tone, an optional bar and detail lines, plus the keys it handles), a request for a line of text (optionally masked), a prompt to send (optionally in a task's own session), or a notice. The interface draws it; the extension never touches the UI, so it would work the same in another interface.

## Configuration

`~/.config/codeit/codeit.json` for you, and `codeit.json` or `.codeit/codeit.json` in a project (folders from the git root down to the working directory; later ones win). Comments are allowed, and `{env:NAME}` is replaced by an environment variable. The keys have the same shape as opencode's, so sections can be copied from `opencode.json`:

```jsonc
{
  "model": "copilot/claude-sonnet-5",
  "small_model": "copilot/gpt-5-mini",   // condensing and summaries (default: the session's model)
  "permission": {
    "bash": { "*": "ask", "git status*": "allow", "cargo test*": "allow" },
    "edit": "allow",
    "webfetch": "ask"
  },
  "instructions": ["docs/conventions.md", "~/notes/style.md"],
  "skills": { "paths": ["~/workspace/my-skills"] },
  "command": {
    "fix-ci": { "description": "fix the CI failure", "template": "CI fails with:\n!`gh run view --log-failed | tail -50`\nFix it." }
  },
  "agent": {
    "docs": { "description": "writes documentation", "mode": "subagent", "prompt": "You write clear docs.", "tools": { "bash": false } }
  },
  "mcp": {
    "tracker": { "type": "remote", "url": "https://mcp.example.com/mcp", "headers": { "Authorization": "Basic {env:TRACKER_BASIC}" } },
    "files": { "type": "local", "command": ["npx", "-y", "@modelcontextprotocol/server-filesystem", "."] }
  },
  "compaction": { "auto": true, "prune": true, "reserved": 20000 },
  "tool_output": { "max_lines": 400, "max_bytes": 24000, "condense": true, "condense_min_tokens": 2000 },
  "shell": "/bin/bash",
  "default_agent": "build",              // the agent new sessions start with
  "approval": "auto",                    // or "ask", for new sessions
  "tui": { "mouse": true,                // false: no mouse capture (plain terminal selection)
           "background": "#1e1e1e" }     // the screen's gray; "none" keeps the terminal's
}
```

Environment variables, all optional:

| Variable | Purpose |
|---|---|
| `CODEIT_DEBUG=1` | Log requests and errors (never tokens) to `~/.cache/codeit/debug.log` |
| `CODEIT_DEBUG_BODY=1` | Also log request bodies |
| `CODEIT_BROWSER` | Program to open URLs with (logins), instead of the system browser |
| `CODEIT_NO_CATALOG=1` | Don't load the models.dev catalog (only Copilot and the GLM Coding Plan) |
| `CODEIT_SPLASH=off` | Skip the logo animation when the interface opens (any key skips it too) |
| `CODEIT_DEMO=1` | Add a scripted `demo` provider (no account needed) for trying the interface; see Development |
| `CODEIT_COPILOT_TOKEN` | Use this GitHub token for Copilot instead of a saved login |
| `CODEIT_COPILOT_CLIENT_ID` | GitHub OAuth app for the device login (default: OpenCode's) |

## How the providers work

**Catalog providers.** Each is spoken to in its API's shape: OpenAI's `/responses` for OpenAI itself, Anthropic's `/v1/messages` for Anthropic (and providers built on its SDK), OpenAI's `/chat/completions` for the rest (Google through its OpenAI-compatible endpoint). Models come from the catalog, keeping those that can call tools; a local server's come from its own `/models`. With a ChatGPT login, OpenAI requests go to ChatGPT's Codex endpoint instead, with the models opencode allows for it.

**Copilot.** GitHub's device flow gives a GitHub OAuth token, which is sent to `api.githubcopilot.com` (or `copilot-api.<your GHE domain>`) as the bearer token. codeit reads `/models` and offers the models Copilot marks as picker-enabled. Each model goes to the API Copilot lists for it: `/chat/completions`, `/responses` (GPT-5 class) or `/v1/messages` (Claude), each with its own tool-call format. Reasoning is sent back the way each API needs it during a tool loop: Claude's signed thinking blocks, GPT-5's encrypted reasoning (`store: false`), Copilot's `reasoning_opaque`, and only to the model that produced it. Claude requests mark the system prompt and the last messages for prompt caching. Requests carry the same headers OpenCode sends: `X-GitHub-Api-Version`, `X-Interaction-Id` (one per session), `Openai-Intent`, and `x-initiator` (`user` for what you type, `agent` otherwise).

## Troubleshooting

| Message | Meaning |
|---|---|
| `Copilot 401` | Run `codeit login copilot` again. |
| `Copilot 403` on one model | Enable that model in your GitHub Copilot settings. |
| `<Provider> 401` / `403` "…key was rejected" | Run `codeit login <provider>` with a current key. |
| A provider is missing from `codeit providers` | The catalog couldn't be downloaded; check `https://models.dev/api.json` is reachable, then delete `~/.cache/codeit/models.json`. |

## Development

```bash
cargo test
cargo clippy --all-targets
cargo fmt --all
```

To see the interface without spending requests, run `CODEIT_DEMO=1 codeit` in a scratch folder with `printf 'def add(a, b):\n    return a - b\n' > calc.py`, pick a `demo` model, and send anything: it plays a short scripted session (reads, an edit, a long failing command, a passing one, a reply) through the real harness. It is in `crates/codeit-providers/src/demo.rs`.

`crates/codeit-harness/tests/agent_loop.rs` runs the whole loop against a scripted provider: tool calls, permissions, subagents, plan mode, undo. Provider wire formats have unit tests next to them. To change the system prompt or a tool description, edit `crates/codeit-harness/src/prompts/` or the tool's `spec`; keep them short, since they are sent with every request.

### Layout

```
crates/
  codeit-providers/    model providers behind one `Provider` trait (text, reasoning, tool calls)
    src/copilot/       GitHub Copilot (port of opencode's Copilot plugin)
    src/zai.rs         Z.ai GLM Coding Plan (OpenAI-compatible, API key)
    src/catalog.rs     every other provider opencode lists (models.dev), with an API key
    src/chatgpt.rs     OpenAI with a ChatGPT Plus/Pro login
    src/protocols.rs   the three API shapes: /chat/completions, /responses, /v1/messages
  codeit-harness/      the agent: loop, tools, permissions, sessions, context, MCP, extension API
    src/tools/         read, write, edit, apply_patch, bash, grep, glob, todo, task, skill, webfetch, question
    src/prompts/       the system prompt and the other prompts, as Markdown
  codeit/              the TUI binary and the non-interactive commands
```

## License

[MIT No Attribution](LICENSE): use it for anything, no strings attached. Parts are ported from opencode (MIT) and Codex (Apache-2.0); their notices are in [THIRD_PARTY_NOTICES](THIRD_PARTY_NOTICES).
