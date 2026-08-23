# Simplicity

Complexity judgment for maintainability — the depth behind the **Complexity** item in `QUALITY-BAR.md`. Everything here answers one question: what will the next person pay to change this?

## What complexity is

Complexity is what someone must hold in their head to change something correctly: ordering, lifetimes, error semantics, thread rules, which states are legal, which combinations are not.

Two populations hold it — whoever works inside an abstraction, and everyone outside it. Simplicity is a claim about the outside population only: how much they must know. A change that makes every caller check a new flag added complexity; a change that lets every caller stop tracking initialisation order removed it.

Essential complexity is never deleted, only placed — in a boundary, in a spec, in an earlier stage, in a pin. A simple outside is paid for inside the abstraction — absorbing what the outside no longer has to know. That cost is legitimate: the systems that let everyone else abstract a problem away took on the essential complexity themselves. Their implementations are harder, not easier. An implementation that sheds work its callers then have to take on has simplified nothing.

## Context decides

None of it is answerable without context: who the callers actually are, what the spec requires of them, which states an earlier stage already excluded. Establish that first — until it is, there is nothing to weigh and the judgment has no content.

Unclear context is itself a source of complexity: hedges — extra cases, flags, layers — the code carries because the spec, the callers, or the legal states were not yet known. Establishing that context places the constraint where it already lived. The hedges can go. That is not Dropped: no requirement went away.

Where the context does not exist — an open caller set, a spec that is silent — the gap is the finding. Name what you would need to know to place the boundary, and hand that back rather than inventing a boundary to fill the silence.

If the change did not add, move, or leak a boundary, there is nothing to place. That is a complete answer.

## Necessity — the boundary in both directions

A boundary earns its place from cases that exist today, in the context you established. Locating that cut is the work. Both failures are failures of necessity.

**Too little.** The outside must know things that belong to the problem rather than to it. The test that separates a contract item from a leak: would this still hold if the inside were reimplemented? What survives reimplementation is contract; what does not, leaked.

**Too much.** A boundary nothing needed, taxing every future reader with a concept that buys nothing. The signals are concrete — an interface with one implementation and one caller, a layer you cannot explain without inventing vocabulary, a parameter serving a case no caller has.

The cut that governs defensive code governs abstraction too: does something *prevent* the case, or has it merely *not come up yet*? A case the context excludes is asserted and pinned, not implemented. The second real case is the trigger to stop and design the boundary — past that point, each case absorbed as it arrives is another layer nobody chose.

Pin both sides, per `COMMENTS.md`. The abstraction you did not build needs a pinned reason as much as the case you did not handle; the abstraction you did build needs the real cases that justify it.

Uphill is: establish the context, then contain what necessity justifies. Reaching a simpler outside by deleting essential complexity rather than containing it does not hold — what was dropped comes back one special case at a time. Deep interiors — compilers, drivers, operating systems — are frequently correct and unavoidable; their risk is the abstraction that cannot say what it will not do. A simpler outside at system scale is maintainability, never novelty for its own sake.

## What cannot be deleted

Essential complexity moves; it does not vanish. When a change appears to remove it, one of two things happened: it moved somewhere you have not looked, or a requirement was dropped. Establish which. Some complexity only looks essential — phantom, inherited from how the problem was framed, or from hedges written before the context existed — and removing that is the real win. A requirement that quietly went away is a correctness defect, not a maintainability one; it outranks everything in this document and is reported there.

Accidental complexity that escapes a boundary becomes essential for whoever must now interact with it. It is no longer yours to remove, because removing it breaks a neighbour that had no choice but to depend on it. The leverage sits at the moment a seam is created, while nothing outside depends on it yet. Afterwards, the cost of moving a boundary scales with the number of parties holding it. Accreted terrain is navigated and pinned, not cleaned in passing — a cleanup that has to move a boundary is a design decision, and it belongs at the design gate.

## What to produce

Not a verdict — a placement. Skip this when there is nothing to place.

- Where the boundary sits, and the cases that exist today which justify it.
- What the outside must know afterwards, with the call sites that hold it.
- What was absorbed inside to buy that.
- What context you could not establish.

Three shapes the measurement takes, worth recognising by name:

**Leaky** — the interface reads cleaner, and the outside must know just as much. The load moved rather than went.

**Sediment** — the list grew an item at a time, each cheap alone, and the next change pays for all of them.

**Dropped** — the list got shorter because a requirement went away rather than being contained. Check it against *What cannot be deleted* before filing it as a maintainability finding.

"No clean boundary exists here; this is the least bad placement, and here is what it costs" is a complete answer. So is "this should not be touched."
