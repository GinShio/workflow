---
name: design-protocol
description: Guide main-session design work for architecture, interfaces, trade-offs, refactoring strategy, or non-trivial engineering decisions. Use when the user asks to design, plan, structure, think through, or evaluate an approach before implementation.
---

# Design Protocol

This is the **Design gate** of the working agreement, run in full. Your job is to make the user's decision well-informed, not to make it for them — the user owns the decision.

Every trade-off you weigh is weighed against `craft/QUALITY-BAR.md`. Read it before proposing anything, and name the item a trade-off actually costs. When options touch seams, abstractions, or refactors, read `craft/SIMPLICITY.md` and place the complexity each option carries.

## The loop

The arc is **fractal**: any decision may contain sub-problems that need their own pass through these five, and the same five repeat at every scale. Don't force a clean sequence.

1. **Frame** — the Understand gate. Read the existing code, interfaces, docs, and constraints before proposing anything about them. Restate the problem, and surface each assumption explicitly ("I'm assuming X here — is that right?"). Ask only where the problem statement is ambiguous in a way that leads to fundamentally different designs.
   *Done when the problem, its constraints, and every assumption you are resting on are stated where the user can correct them.*

2. **Explore** — map the viable approaches with their costs, benefits, and failure modes; present 2-3 when the space is genuinely open. Ground every option in cases that exist today — a need the user has not mentioned is an assumption to raise, not a requirement to design for.
   *Done when every approach that survives the constraints is on the table, each with the bar item its cost lands on named and the complexity it carries placed per `craft/SIMPLICITY.md`.*

3. **Narrow** — cut the options the constraints kill, and name the axes that decide the rest. Say "this depends on X" wherever it does, and name X.
   *Done when each surviving option is tied to the condition under which it wins.*

4. **Converge** — help the user land on a decision they could defend to a colleague. The failure mode to watch for in yourself is **premature convergence**: landing on an answer before the space has been explored, then dressing the landing up as analysis. State a leaning as a leaning, with its reason, and leave the options standing beside it. Where two approaches are genuinely equivalent, say so — that is a finding, not a failure to decide. Then ask for fresh eyes: "this is what I have — what do you see that I didn't?"
   *Done when the user has made the call, or has named what blocks them from making it.*

5. **Document** — produce the converged output below.
   *Done when every decision carries its reason, every rejected option carries why, complexity for the chosen path is placed per `craft/SIMPLICITY.md`, every unverified fact sits in Open items, and every place the implementation will look strange is listed for pinning.*

## After convergence

Implementation and review will surface gaps neither of you anticipated. This is normal, not failure. Decide whether the gap takes a local fix or its own pass through the loop — most non-trivial gaps take the pass — and re-enter at the step that owns it: sometimes a clarification back in Frame, sometimes an assumption that unravels all the way to Explore.

Watch for the gap that keeps connecting to other things. A "nit" that touches three unrelated places is a foundational gap in a small disguise: address the principle, not the instance.

## Stance

Two situations change how you run the loop:

**The user arrives with a direction already chosen.** Stress-test it rather than execute it: what does the approach cost, what happens when the input is empty or the queue is full or the spec changes, which edge cases are missing. Find the cracks before reality does — and challenge the framing itself when a better problem is hiding behind the stated one.

**The user is stuck.** Change the shape of the question: look at it from another component's perspective, separate A from B and solve them independently, find how a comparable system solved something similar, or ask what the minimum viable version looks like.

## External facts

Design reasoning may rest on facts from the repo, from the user, or from research Puppet already did. Load-bearing external facts are established, never assumed.

When a design depends on a spec requirement, upstream behavior, hardware fact, benchmark, prior art, ABI constraint, or any other external claim local context does not settle, hand it to Puppet or ask the user to approve the research path before converging. State the exact fact you need and why it matters. A fact that stays unverified belongs in Open items, not baked into the decision.

## Output

For open design discussion:

```markdown
## Problem Space
[What we're solving, why, and what constraints exist]

## Approaches
### Option A: [name]
- **How:** [mechanism]
- **Pros:** [what it does well]
- **Cons:** [what it costs or risks, named against the bar item it costs]
- **Complexity:** [what the outside must know under this option, and what it freezes — per `craft/SIMPLICITY.md`]
- **When it wins:** [the scenario where this is clearly right]

### Option B: [name]
- **How:** ...
- **Pros:** ...
- **Cons:** ...
- **Complexity:** ...
- **When it wins:** ...

## Key Decision Points
[What the choice really depends on — the axes that matter]

## Analysis
[Trade-off analysis grounded in the user's constraints. The user decides.]
```

For converged designs:

```markdown
## Decision: [what was chosen]

**Rationale:** [why, grounded in constraints and trade-offs]
**Rejected:** [what else was considered and why not]
**Complexity:** [where the boundary sits and what necessity justifies it; what the outside must know afterwards; what is absorbed inside]
**Risks:** [what could go wrong, what assumptions might be wrong]
**Context to pin:** [parts of the implementation that will look strange without design context — the implementer turns each of these into a context-preserving comment]
**Open items:** [what still needs deciding or investigating, and any fact still unverified]
```
