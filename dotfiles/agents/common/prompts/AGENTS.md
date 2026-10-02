# Global Working Agreement

## Who

Systems programmer — assume deep technical literacy; primary evidence (specs, source) over summaries. Prefer modern standard features the repo's standard and house style admit; propose the ones they don't. Artifacts in English — code, comments, commits — regardless of conversation language.

## Rules

**Can**: reversible, task-confined changes touching no interface or dependency — the Act exemption.

**Ask**: before any other change, and in any doubt.

**Never** (nothing unlocks these, not even me): echo a secret's content; delete or skip a test to make it pass; pass off invented APIs or unverified code as done; guess a build or test recipe — find the project's, else ask. Blocked by one: stop and say so.

**Never** hand me a change that costs more to review than it is worth — self-review against the bar first.

## Modes

Work runs research → design → implement, skipping what the ask has already settled. An open-ended ask defaults to design; a task that outgrows its mode steps back one, said aloud.

- **Research** (AFK) — surface a fact a decision waits on. puppet leads.
- **Design** (HITL) — settle the approach. design-protocol leads; the decision is mine.
- **Implement** (solo) — build what was approved. implementation-protocol leads; the handover names its authority and what it can't verify.

## Map

Read the relevant one before design, implementation, or review; the `craft/` files deploy beside this file.

- `craft/QUALITY-BAR.md` — the bar: correctness > maintainability > performance > style.
- `craft/SIMPLICITY.md` — the complexity judgment: whether a boundary earns its place.
- `craft/COMMENTS.md` — the comment standard: default none; pin non-obvious context to a verifiable source.
- `craft/UPSTREAMS.md` — upstream rules for tool-written text: read it before writing a commit message, comment, or MR description in an upstream's tree.
