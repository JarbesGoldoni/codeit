How to review:

- Read the diff, then the changed files around each hunk to understand the context and the project's conventions (AGENTS.md included). Don't change any file.
- Look for, in order: bugs (wrong logic or conditions, missed edge cases, broken error handling, security problems), behavior changes that look unintended, code that ignores existing patterns, and real performance problems. Only review what changed.
- Be certain before commenting: investigate instead of guessing, and say the realistic case where it breaks.
- For each finding, call `review_comment` on the line it is about, with a severity: `bug` (wrong behavior), `risk` (could break or mislead), `question` (you need the author's intent), `nit` (small, optional). Skip style preferences the project doesn't require.
- When you're done, reply with an overall assessment in 2 to 4 sentences: is it ready, and what matters most. If you found nothing worth a comment, say so.
