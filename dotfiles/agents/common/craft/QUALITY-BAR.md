# Quality Bar

What counts as good work — the same four items whether you are designing, implementing, or reviewing.

They rank strictly: a correctness bug outranks a maintainability concern, which outranks a performance concern, which outranks a style nit. When two conflict, the higher one wins and the trade is stated out loud rather than made silently.

## 1. Correctness

Does it do what it claims, on every path?

Reason explicitly about edge cases, lifetimes, ownership, error paths, integer widths, alignment, and concurrency, and state your assumptions. Every error path resolves — propagated, converted, or cleaned up and returned; a discarded error is a correctness bug. Every allocation traces to one owner and one release reachable from every exit.

Code that looks wrong may be correct under a constraint you cannot see from here; establish the constraint before judging it.

Doing what was asked is part of this. A change can be free of bugs and still be the wrong change — implementing what no one requested, or contradicting the spec it claims to follow.

## 2. Maintainability

Cheap for the next person to change — not merely short. Judge by one question at two scales: what will it cost them to change this?

**Complexity** — the main work here. Every change places complexity deliberately: at a boundary necessity justifies, with what the outside must know stated and pinned. Judgment: [`craft/SIMPLICITY.md`](craft/SIMPLICITY.md).

**Within a module** — separate concerns, prefer clarity over cleverness, name for intent. Split analyse-and-transform into two passes unless profiling says the merge matters.

**Context** — decisions that look arbitrary without background are maintainability defects until pinned. Contain what you can in design; pin the rest per `COMMENTS.md`.

## 3. Performance

Understand why the cost exists before touching it. Refactor toward an efficient, elegant implementation; a cleaner design that runs slightly slower is the right trade unless profiling proves otherwise. Solve 95% of the hot path and leave the design unwarped by the barely-visible 1%.

## 4. Style

Follow the project's established patterns: read neighbouring code, build files, and config before introducing one. The project's conventions decide, and preferences from outside it do not. Formatting that a formatter handles is not a quality question.

---

Work is ready when every item above has been applied to it and everything you could not establish is stated plainly.
