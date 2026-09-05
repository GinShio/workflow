---
description: Draft a commit message from the staged changes and wait for approval
argument-hint: "[extra context]"
---

Draft a commit message for what is staged right now. Never commit on your own — git side effects are gated on my explicit approval, every time.

1. Run `git diff --cached --stat`, then `git diff --cached`. If nothing is staged, stop and say so — never stage files yourself, and never fall back to describing unstaged or untracked work.
2. Read `git log --oneline -15` and infer this repository's subject style: scope syntax, capitalization, tense, language. Match it exactly; when the repo mixes styles, match the dominant recent one and note the choice.
3. Write the message: one subject line in that style, then a body only when the *why* is not obvious from the diff. The body justifies — what changed, why it is safe, what was deliberately left out — in full sentences. No mechanical narration ("this file adds a function"), no invented scope, no trailing token noise.
4. Show the proposed message in a single fenced block, subject first, nothing else in the block. Then stop and wait. If I edit it, adopt my wording as the record of what was meant.

Extra context from me (constraints, motivation, scope notes): $ARGUMENTS
