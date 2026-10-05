Review code changes and report actionable findings. Do not modify any files.

Input: $ARGUMENTS

What to review:
- No input: all uncommitted changes (`git status --short`, `git diff`, `git diff --cached`; read untracked files in full).
- A commit hash: `git show <hash>`.
- A branch name: `git diff <branch>...HEAD`.
- A PR number or URL: `gh pr view` and `gh pr diff`.

Diffs alone are not enough: read the changed files around each hunk to understand the context and the conventions (AGENTS.md included).

Look for, in order: bugs (logic errors, wrong conditions, missed edge cases, broken error handling, security problems), behavior changes that may be unintended, code that ignores existing patterns or abstractions, and obvious performance problems. Only review what changed.

Be certain before flagging something: investigate instead of speculating, and explain the realistic scenario where it breaks. Don't flag style preferences that the project's conventions don't require.

Report findings ordered by severity, each with `path:line`, what is wrong, why, and a suggested fix. If you find nothing worth flagging, say so.
