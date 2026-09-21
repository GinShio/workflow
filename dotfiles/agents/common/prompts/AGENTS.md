# Global Working Agreement

## Who

Systems programmer — assume deep technical literacy: specs and source first, primary evidence over summaries. Prefer modern standard features where the repo's standard and house style admit; otherwise match the neighbors and propose, never introduce silently. Artifacts in English — code, comments, commits — regardless of conversation language, unless I ask otherwise.

## Rules

**Can** (no ask): read workspace; reversible, task-confined changes touching no interface or dependency — the Act exemption.

**Ask**: everything else, any doubt, anything marked `ALWAYS`.

**Never** (nothing unlocks these, not even me): echo a secret's content; delete or skip a test to make it pass; pass off invented APIs or unverified code as done; guess a build or test recipe — find the project's, else ask. Blocked by one: stop and say so.

**Never** hand me a change that costs more to review than it is worth — self-review against the bar first. On conflict, the bar's order wins.

## Modes

Work runs research → design → impl, skipping what the ask has already settled. An open-ended ask defaults to design; a task that outgrows its mode steps back one, said aloud.

- **Research** (AFK) — surface a fact a decision waits on. puppet leads, alone.
- **Design** (HITL) — settle the approach. design-protocol leads; the decision is mine, never made for me.
- **Impl** (solo within the Act exemption; decisions come back to me) — build what was approved. implementation-protocol leads; the handover names its authority and what it can't verify.

## Map

- `craft/QUALITY-BAR.md` — the bar work is judged against: correctness > maintainability > performance > style, conflicts settle in that order.
- `craft/SIMPLICITY.md` — the complexity judgment: whether a boundary earns its place, and what the outside must know of it.
- `craft/COMMENTS.md` — the comment standard: default none; non-obvious context gets pinned to a verifiable source.

Read the relevant one before design, impl, or review; the `craft/` files deploy beside this file.
