# Comments

The standard for every comment you write and every comment you review.

Default to no comment: rely on self-documenting names and structure. When a comment feels necessary for simple code, first try renaming or restructuring until it is redundant. Code is the source of truth, and a comment that duplicates it will drift away from it.

## The three tiers

**Inline** (inside a function) — explains *what* and *why*. For simple code the *how* is the code itself, so restating it is noise. Inline comments exist for the context the code cannot express: spec requirements, hardware constraints, non-obvious invariants.

**Summary** (function header, block) — may describe *how* when the logic is genuinely complex: a multi-step algorithm, a state machine, a pass with non-trivial invariants. It orients a reader before they descend into the detail.

**API / interface** (public headers, docstrings, trait docs) — documentation for *consumers*, not for readers of the implementation. Describe the contract in full: what it does, preconditions, postconditions, invariants, error behaviour, thread-safety, lifetime requirements, and edge cases. A behavioural detail missing here becomes a bug in a caller.

## Name intent, not code

Describe intent at the level of abstraction the reader needs. Code is dynamic: a comment that names a variable, function, or type silently starts lying the moment that name changes.

Stable external names are the exception, and belong in the comment when they are the thing being documented: public APIs, spec sections, extension names, ABI contracts, issue IDs, and hardware errata.

## Context-preserving comments

Some code looks strange to any reader who lacks context the code cannot hold. That context must be pinned. The recurring triggers:

- A spec-mandated ordering, sequence, or value that the code alone does not reveal.
- A hardware intrinsic, or a sequence of them, that looks arbitrary.
- A translation from a higher-level spec (GLSL to SPIR-V, NIR to ACO) where the mapping is non-obvious.
- A hardware errata workaround.
- An IR invariant that constrains which transformations are legal at this point.
- A seemingly redundant operation — a barrier, fence, or flush — that exists for a reason the code does not show.

**Pin the claim to a verifiable source.** A reader must be able to check it and understand why the code cannot be simplified or reordered.

- Good: `// Vulkan spec §7.4: pipeline barrier must precede the first draw accessing this image.`
- Good: `// OpControlBarrier semantics require execution + memory dependency here.`
- Good: `// Navi2x hardware bug: depth decompress must precede color resolve (mesa!12345).`
- Weak: `// This is needed.` — says nothing.
- Weak: `// Hardware requirement.` — which hardware, which requirement?
- Weak: `// Increment buf_count.` — restates the code and will drift.
- Weak: `// Don't remove this.` — why not, and what breaks?

A comment in this category that is missing, or present but unverifiable, is a defect in its own right — not a lesser observation about wording.
