---
name: implementation-protocol
description: Guide main-session implementation work for features, bug fixes, refactors, and tests. Use when the user asks to implement, fix, add support, refactor according to an approved plan, or write code.
---

# Implementation Protocol

This is the **Implement gate** of the working agreement, run in full, with the two gates before it as entry conditions. You implement approved designs — you are not designing through implementation.

Read `craft/QUALITY-BAR.md` and `craft/COMMENTS.md` before writing code, and apply every item of both.

## Understand gate

1. Read the existing code — what's there, and the conventions it follows.
2. Read the neighboring tests — the testing patterns and the coverage expected.
3. Trace the blast radius — which files change, and who depends on what you are about to touch.
4. **Build and test: check project context first, then ask.** Look for build/test/lint instructions in the project-level AGENTS.md/CLAUDE.md, README, or CONTRIBUTING. If found, follow them. If not found, ask — C/C++ build systems are too varied to guess reliably from config files alone. Investigate build files on your own only when the user tells you to.

Ask when the task is ambiguous about scope or intent.

*Done when you can state which files change, where the blast radius ends, and the exact commands that build and test the result.*

## Design gate

Work from an approved design. This is non-negotiable for non-trivial changes.

- No design exists: say "this needs a design discussion before I implement" rather than designing through code.
- Implementation-level decisions (algorithm choice within a function, variable naming): make the call and note it in your output.
- Architectural decisions (new module, interface change, new dependency, data format, build/config change, security- or concurrency-sensitive behavior): STOP and confirm first — these are design decisions disguised as implementation details.

*Done when every architectural decision the change contains traces back to something the user approved.*

## Plan the work

Multi-phase or multi-file work gets a task plan before the first edit: discrete tasks in execution order, each carrying a **verification criterion** — the concrete check that proves it done, usually a command and the output that counts as green. Record the plan in the harness's task tool where one exists. Work one task at a time, closing each before opening the next. A task you cannot write a criterion for is a design question — take it back to the Design gate instead of implementing through it. A single-file change needs no plan.

*Done when every task names its criterion, and every closed task's criterion has run for real.*

### Closing a task

1. **Run the criterion** — real output seen, not an expected green.
2. **Boundary re-read** — re-read what the task wrote at the boundaries: each `unsafe` block against the invariant it asserts, each call crossing a language or module boundary, each site marshalling to another representation. Check each against the pattern the surrounding code already uses; where the correct pattern sits ten lines away, a divergent neighbour is a bug, not a style difference.
3. **Record the residue** — anything the task could not verify goes to the handover's **Not verified**, never dropped silently.

*Done when the criterion's output exists, the boundaries have been re-read against their neighbours, and the residue is written down.*

## Write the code

1. **Keep changes focused** — one logical change per unit. Refactoring and feature work stay in separate units. Present large changes in reviewable chunks rather than one 500-line diff.
2. **Verify thread safety** — where data is shared, establish lock ordering and release on every path. Where local context cannot establish it, pin the assumption you relied on.
3. **Treat generated output in the build tree as a debugging clue.** To change it, edit the *generator*, not the product.

**The 2-3 failure rule:** after 2-3 failed attempts at the same obstacle, STOP. An ugly workaround forced through is a design signal ignored. Re-question the design: "I've tried X, Y, Z and they all fail because [root cause]. The design may need adjustment."

### Cases that cannot occur

Handling a case the contract excludes is not extra safety. It weakens the contract — the next reader can no longer tell whether that case is allowed — and defensive code spreads outward from there.

So narrow what you handle only on a constraint you can **pin**: an interface contract, a spec clause, an invariant an earlier stage establishes. The question that separates the two lookalikes is whether something *prevents* the case, or the case merely *has not come up yet*. A documented invariant an earlier stage establishes licenses skipping it; "no input I have seen does this" does not — that case is unhandled, not impossible. When you cannot pin it, handle the case.

*Done when every path you touched resolves — each error handled, each allocation released, each shared access ordered — and every case you chose not to handle carries a pinned constraint.*

## Self-review

Apply every item of `craft/QUALITY-BAR.md` to what you wrote, and pre-empt what the user would catch. Three checks deserve naming because they are the ones most easily skipped:

- **Context preservation** — scan for anything that would make a reviewer ask "why is this here?". Where the answer lives outside the file, pin it per `craft/COMMENTS.md`.
- **Complexity** — for each boundary you touched, state what the outside must know to use it and name the call sites that hold it (see `craft/SIMPLICITY.md`). If you placed none, say so.
- **Authority** — which gate authorized this? Either an approved design, or the *Act* exemption it qualified under. If neither holds, you have overrun the loop; stop and say so.

Then verify what you can (build, test, lint where a recipe exists), handing over **evidence, not assertion**: the command with its green output, and every skipped check named as skipped. A green suite is evidence only for defect classes the environment can observe — a driver that skips validation, or an allocator that has not yet reused a freed page, lets broken code pass. For each class the environment cannot see, fall back to the spec or contract text. Prefer checks that are cheap and side-effect-free; leave costly or stateful verification to the user.

*Done when every bar item has been applied to every file you touched, every question a stranger to the change would ask is answered in the code or pinned beside it, and the handover names its authority and everything you could not verify.*

When asked to commit, the commit message standard and the attribution trailer to attach are in [`commit.md`](commit.md).

## Output

```markdown
## Changes Made

**Scope:** [what was changed and why]
**Authority:** [the approved design this implements, or the Act exemption it qualified under]

**Files modified:**
- `path/to/file.cpp` — [what changed and why]

**Implementation decisions:**
- [decision made and why — things you chose that could have gone differently]

**Narrowed:** [any case deliberately left unhandled, and the constraint pinned for it]

**Complexity:** [what the outside must know after this change, with the call sites holding it; what was absorbed inside; any boundary this froze — or that no boundary was placed]

**What to review:**
- [areas deserving careful attention — where bugs are most likely]

**Verified:** [commands run and their output — pasted; skipped checks named]
**Not verified:** [what couldn't be checked and why]
```
