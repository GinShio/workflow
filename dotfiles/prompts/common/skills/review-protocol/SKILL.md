---
name: review-protocol
description: The quality bar and method for reviewing code and designs. Use when reviewing changes, a diff, a design, or a subsystem for correctness, design, maintainability, performance, and style — including conversational, back-and-forth review and review right after writing code. This skill is the single source of truth for review judgment.
---

# Review Protocol

Review against the quality bar, not personal taste. Understanding the change is the work; findings are what remains after that. Respect the author's design decisions while challenging execution. Output is a verdict and notes, not patches: **LGTM** when the bar is met, otherwise a short note with the direction of a fix — the author writes the patch. If you were asked to review, review, including the tests when they are in scope. The author is the reader of every note, on an MR and in conversation.

Read `craft/QUALITY-BAR.md` and `craft/COMMENTS.md` before reviewing. When the change adds, moves, or leaks a boundary, apply maintainability complexity judgment per `craft/SIMPLICITY.md`. This skill is the judgment for applying them.

Review is conversational: share findings, take the user's pushback, re-examine, and refine over as many rounds as it takes. Workflow-specific mechanics (for example the local `wits review` flow) live in their own skill and defer to this one for judgment.

## The review

1. **Understand the change** — the design or intent behind it, the code around it, the project's conventions, and the spec it answers to where one exists. Read changed files whole rather than only their diffs; a diff hides what the change assumes about everything it didn't touch.
   *Done when every changed file has been read in full, and you can state what each changed region does, what it assumes about the code it didn't touch, and how a caller or a later pass depends on it.*

2. **Assess in the bar's order.** One pass through the bar is enough — separate passes per item are not required. Correctness comes first, and code that looks wrong may be correct under a constraint invisible from here, so run the context-dependent protocol below before flagging it. Where a spec exists, check compliance as its own axis. When the change adds, moves, or leaks a boundary, place that complexity per `craft/SIMPLICITY.md`; a change that places no boundary has nothing to place.
   *Done when every bar item has been applied to the change you understood, complexity is placed only where a boundary moved, and each finding — if any — names the item it lands on.*

3. **Rank and calibrate.** Group findings by severity, and order by the bar within each group. Ask of each candidate: would you want the author to change the code for this? A Suggestion earns its place only when it would still be worth saying on an otherwise LGTM patch.
   *Done when every finding carries a severity you would defend out loud, nothing sits in Blocking that you would not actually block on, and a change with no such finding is LGTM.*

4. **Write the review.** Each finding is a short note the author can act on: what is wrong, which way to go. Add an example when the direction isn't obvious from the note — a call that already does it, a type, a return, a `[[path:line]]`. LGTM is one or two sentences: what the change is, and that it meets the bar. Raise an alternative only when it is clearly better, and say what makes it better; raise a performance finding only when you can name the path that makes it matter. Ask about intent only where the surrounding code doesn't answer it. Omit empty severity sections.
   *Done when the Summary is LGTM or names what needs attention, and every finding is a short note the author can act on.*

## Priority and severity

Findings rank by the bar — correctness, then maintainability, then performance, then style. A finding inherits the rank of the item it lands on, then moves within that rank by how much damage it does.

- **Blocking** — you would refuse to merge. Correctness bugs, safety issues, spec violations (a simplification that dropped a requirement is one), data-loss risks.
- **Important** — you would push back hard. Design problems, and maintainability defects: mixed concerns that belong apart, unpinned context on non-obvious code, complexity failures per `craft/SIMPLICITY.md` (leaky, sediment, boundaries nothing justifies), missing tests on important paths. These are not casual suggestions; they compound over time and end in code nobody can change.
- **Suggestion** — could improve, and would still be worth saying on an otherwise LGTM patch. Names that don't carry intent, minor structural improvements, extra tests for edge cases. No immediate pain, real help to future readers.
- **Nit** — truly trivial. Under thirty seconds to fix, not worth arguing about.

## Spec compliance

A change can be correct — no bugs — and still fail to implement what was asked for. That makes compliance its own axis, not a correctness sub-item. Skip it when the author had no spec.

For every requirement in the spec, ask three questions:

| Question | What it catches |
|---|---|
| **Missing:** did the spec ask for this? | Requirements that weren't implemented |
| **Creep:** did the spec not ask for this? | Scope creep — code implementing something the spec didn't request |
| **Wrong:** does the implementation match what the spec says? | Requirements that look implemented but contradict the spec |

Quote the spec line for each finding. Spec ambiguity is the spec author's responsibility, not the implementer's — flag the ambiguity as a finding rather than blaming the implementation.

Spec violations are correctness issues, so they go in **Blocking Issues**, tagged `[Spec]` to stay traceable to this axis. Example: `[Spec] Missing: the spec requires rate limiting on line 42, but no rate limiter is wired up.`

## Context-dependent code

When you meet code that looks wrong, strange, or unnecessarily complex, work through this before flagging it:

1. **Check for existing context.** Read the surrounding comments, the function or block documentation, and nearby code following the same pattern. The explanation may already be there.
2. **Check for verifiable constraints.** Does this domain have an external spec, or turn on factual claims about hardware behavior, upstream contracts, or an ABI? Look for spec references, intrinsic names, issue IDs, commit messages, hardware-specific patterns. The constraint may be verifiable even where it isn't commented.
3. **Context exists and explains the code:** the code is correct. Now check whether it is *pinned* — would a new team member understand why this can't be simplified? Place the complexity per `craft/SIMPLICITY.md` before calling it unnecessary. A missing or vague pin is a maintainability finding, not a correctness one.
4. **The finding depends on a spec or fact you cannot verify locally:** ask, or hand the question to Puppet. Where the review must proceed without the answer, flag it as needing clarification, with your reasoning: "This appears to [X], which would be [problem]. In [domain] this may instead be required because [possible constraint]. Is this intentional?"
5. **No plausible constraint explains it:** flag as a correctness or design issue per normal review.

**Unhandled cases cut both ways.** Before flagging a case as unhandled, check whether the contract excludes it — a contract-excluded case that gets handled anyway is itself a finding, because it blurs what the contract admits. Where the author narrowed deliberately, the finding is about the pin: a narrowing resting on a documented invariant is sound, one resting on "it hasn't come up yet" is a latent bug.

## Output format

Include only sections that have content. LGTM is a Summary — and What's Good when there is something specific to name. Findings follow the same shape in conversation and on an MR: one short note per finding, an example only when the note needs it.

```markdown
## Review: [scope — what was reviewed]

### Summary
[LGTM — one or two sentences on what the change is; or what needs attention.]

### Blocking Issues
[Must fix before merge.]

1. **[src/lock.c:42]** — Unlock is skipped on this error path; take the same `goto out` as [[src/lock.c:80]].

### Important Issues
[Should fix. Design problems, significant maintainability defects.]

### Suggestions
[Could improve. Worth mentioning, won't block.]

### Nits
[Trivial. Fix if convenient.]

### What's Good
[A specific thing that is good, when there is one.]
```
