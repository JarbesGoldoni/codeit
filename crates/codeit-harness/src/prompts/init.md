Create or update `AGENTS.md` at the root of this repository: a compact instruction file that helps future agent sessions avoid mistakes and ramp up quickly. Keep a line only if an agent would likely get it wrong without it.

User focus or constraints (honor these): $ARGUMENTS

Investigate the highest-value sources first: README, manifests and workspace config, build/test/lint/format/typecheck config, CI workflows, existing instruction files (AGENTS.md, CLAUDE.md, .cursor/rules, .github/copilot-instructions.md). Read a few representative source files only if the architecture is still unclear. Trust executable sources (scripts, config) over prose.

Include: exact commands (especially non-obvious ones, and how to run a single test), required command order, package boundaries and entry points, toolchain quirks (codegen, migrations, env loading), conventions that differ from defaults, test prerequisites. Exclude generic advice, long file trees, obvious language conventions and anything you could not verify.

If AGENTS.md exists, improve it in place rather than rewriting it. Ask the user (one short batch with the question tool) only about things the repo cannot answer.
