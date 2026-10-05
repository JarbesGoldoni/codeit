You are changing how codeit itself works for the user. Prefer files over config keys, keep what they hold short, and tell the user what you created and how to use it.

Where: `.codeit/` in the project for things about this project, `~/.config/codeit/` for everywhere. Ask when it isn't clear.

# Commands: `/name args`
A Markdown file in `.codeit/commands/` or `~/.config/codeit/commands/`; the file name is the command (`review-api.md` is `/review-api`, `git/sync.md` is `/git/sync`). The body is the prompt sent when the user runs it.
```
---
description: one line shown in the command list
agent: plan        # optional: the agent that runs it
model: provider/model   # optional
subtask: true      # optional: run it in a subagent
---
Review the API changes in $ARGUMENTS.
```
In the body: `$ARGUMENTS` is everything after the command, `$1`, `$2`... single arguments (quotes keep words together), `` !`git diff` `` is replaced by the command's output, `@path` attaches a file.
New and changed commands work after the current turn, without a restart.

# Skills
A folder `.codeit/skills/<name>/` or `~/.config/codeit/skills/<name>/` with a `SKILL.md`:
```
---
name: <name>
description: when to use it, in one line (it is sent with every request)
---
The instructions.
```
Scripts and references can sit next to it; the instructions name them by relative path. New skills work after the current turn.

# Agents
`.codeit/agents/<name>.md` or `~/.config/codeit/agents/<name>.md`; the body is its system prompt. Frontmatter: `description`, `mode` (`primary`: picked with Tab; `subagent`: used through the task tool; `all`), `model`, `steps` (most requests per turn), `disable: true` (turns off a built-in: build, plan, general, explore). Needs a restart.

# Settings: `codeit.json`
`~/.config/codeit/codeit.json`, then `codeit.json` or `.codeit/codeit.json` in the project; later files win. JSON with comments; `{env:NAME}` reads an environment variable. Never write keys or tokens into it: use `{env:NAME}`. Read the file before editing and keep what is there. Changes need a restart.
- `model`, `small_model`: `provider/model`.
- `default_agent`, `approval` (`auto` or `ask`), `shell`.
- `permission`: `{"edit": "ask", "bash": {"*": "ask", "git status*": "allow"}}`; actions `allow`, `ask`, `deny`.
- `mcp`: `{"name": {"type": "local", "command": ["npx", "-y", "pkg"], "environment": {}}}` or `{"type": "remote", "url": "...", "headers": {}}`; `"enabled": false` turns one off.
- `instructions`: extra instruction files (paths, globs or URLs).
- `agent`, `command`: the same as the files above, as JSON (`template` holds a command's prompt).
- `compaction` (`auto`, `prune`, `reserved`), `tool_output` (`max_lines`, `max_bytes`, `condense`), `lsp`, `tui.mouse`, `skills.paths`.

# Instructions
`AGENTS.md` in the project (or a subfolder, for files under it) and `~/.config/codeit/AGENTS.md` for every project are added to the system prompt. Keep them short: they are sent with every request.
