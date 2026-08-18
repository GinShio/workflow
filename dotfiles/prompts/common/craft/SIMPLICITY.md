# Simplicity

Complexity judgment for maintainability — the depth behind the **Complexity** item in `QUALITY-BAR.md`. Everything here answers one question: what will the next person pay to change this?

## What complexity is

Complexity is what someone must hold in their head to change something correctly: ordering, lifetimes, error semantics, thread rules, which states are legal, which combinations are not.

Two populations hold it — whoever works inside an abstraction, and everyone outside it. Simplicity is a claim about the outside population only: how much they must know. A change that makes every caller check a new flag added complexity; a change that lets every caller stop tracking initialisation order removed it.

## Context decides

None of it is answerable without context: who the callers actually are, what the spec requires of them, which states an earlier stage already excluded. Establish that first — until it is, there is nothing to weigh and the judgment has no content.

Where the context does not exist — an open caller set, a spec that is silent — the gap is the finding. Name what you would need to know to place the boundary, and hand that back rather than inventing a boundary to fill the silence.

## What simplicity costs

Complexity is never deleted, only placed. Making the outside simple is paid for in two currencies.

**Inside the abstraction** — absorbing what the outside no longer has to know. This cost is legitimate and expected: the systems that let everyone else abstract a problem away are the ones that took on extraordinary amounts of essential complexity themselves. Their implementations are harder, not easier. An implementation that sheds work its callers then have to take on has simplified nothing.

**In the thinking** — locating the boundary is the work, and it is slow, uncertain, and often inconclusive. This is what the title names: making something simple is very complicated.

## Necessity — the boundary in both directions

A boundary earns its place from cases that exist today, in the context you established. Both failures are failures of necessity.

**Too little.** The outside must know things that belong to the problem rather than to it. The test that separates a contract item from a leak: would this still hold if the inside were reimplemented? What survives reimplementation is contract; what does not, leaked.

**Too much.** A boundary nothing needed, taxing every future reader with a concept that buys nothing. The signals are concrete — an interface with one implementation and one caller, a layer you cannot explain without inventing vocabulary, a parameter serving a case no caller has.

The cut that governs defensive code governs abstraction too: does something *prevent* the case, or has it merely *not come up yet*? A case the context excludes is asserted and pinned, not implemented. The second real case is the trigger to stop and design the boundary — past that point, each case absorbed as it arrives is another layer nobody chose.

Pin both sides, per `COMMENTS.md`. The abstraction you did not build needs a pinned reason as much as the case you did not handle; the abstraction you did build needs the real cases that justify it.

## Which way is uphill

Two axes place any change. Horizontal: how much the outside must know afterwards. Vertical: whether you got there by establishing context and thinking, or by reacting to what you found.

|  | Outside must know a lot | Outside barely needs to know |
|---|---|---|
| **Thought through** | Constructed | Revolutionary |
| **Reacted to** | Accreted | Rebellious |

Uphill is up and to the right: pay the thinking, and contain what necessity justifies. Revolutionary here means maintainability at system scale — the outside pays less to change it — and never novelty for its own sake.

Bottom-right is the trap, and it does not hold: reaching the right-hand side by deleting rather than containing lands there, then slides left as what was dropped comes back one special case at a time. Handling each case as it arrives starts in the bottom-left directly — nothing was deleted, nothing was thought through, and the outside learns one more special case every time.

Top-left is frequently correct and unavoidable — compilers, drivers, and operating systems are legitimately deep. Its risk is the abstraction that cannot say what it will not do.

## What cannot be deleted

Essential complexity moves; it does not vanish. When a change appears to remove it, one of two things happened: it moved somewhere you have not looked, or a requirement was dropped.

Establish which. Some complexity only looks essential — phantom, inherited from how the problem was framed rather than from the problem itself — and removing that is the real win. A requirement that quietly went away is a correctness defect, not a maintainability one; it outranks everything in this document and is reported there.

## The ratchet

Accidental complexity that escapes a boundary becomes essential for whoever must now interact with it. It has changed category: it is no longer yours to remove, because removing it breaks a neighbour that had no choice but to depend on it.

So the leverage sits at the moment a seam is created, while nothing outside depends on it yet. Afterwards, the cost of moving a boundary scales with the number of parties holding it.

This is why accreted terrain resists cleanup. The move there is to navigate it, pin what constrains you, and migrate deliberately — a cleanup that has to move a boundary is a design decision wearing implementation clothes, and it belongs at the design gate.

## What to produce

Not a verdict — a placement:

- Where the boundary sits, and the cases that exist today which justify it.
- What the outside must know afterwards, with the call sites that hold it.
- What was absorbed inside to buy that.
- What context you could not establish.

Three shapes the measurement takes, worth recognising by name:

**Leaky** — the interface reads cleaner, and the outside must know just as much. The load moved rather than went.

**Sediment** — the list grew an item at a time, each cheap alone, and the next change pays for all of them.

**Dropped** — the list got shorter because a requirement went away rather than being contained. Check it against *What cannot be deleted* before filing it as a maintainability finding.

"No clean boundary exists here; this is the least bad placement, and here is what it costs" is a complete answer. So is "this should not be touched."
