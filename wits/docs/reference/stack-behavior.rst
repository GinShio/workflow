.. _stack-behavior:

``wits stack`` — behaviour reference
====================================

The authoritative description of *how the verbs decide what to do*, including
the awkward cases — forks, multi-round edits, deleted branches. The usage
guide (:doc:`/commands/stack`) is for getting things done; the design note
(:doc:`stack-design`) is for why the tool is shaped this way; this reference is
the precise contract, aimed at whoever changes or debugs the logic.

Throughout, an MR is the merge request (GitHub's "PR", GitLab's "MR"); the
noun shown to users is per-host, but everything here calls it an MR.

The machete forest
------------------

The machete file (``<common-git-dir>/machete``, one per repository and so
shared by all its worktrees) records a dependency **forest**: each line is a
branch, and indentation means "sits on top of". It is a forest, not just a
chain — a branch may fork into several::

   main
       A            single child: linear
           B        fork-point (children C, D)
               C    leaf
               D    single child: linear
                   F  leaf

Two properties of the parser matter:

* **Reading is indentation-agnostic.** Only *relative* nesting is read: a
  line's parent is the nearest preceding line with strictly smaller indent.
  Two spaces, four, or a tab all parse identically, and deleting a middle line
  leaves its child correctly attached to the grandparent even if the child is
  not re-indented.
* **Writing normalizes to four spaces.** A rewrite (by ``anno`` caching
  numbers, or ``slice``) emits four spaces per level regardless of the input
  width.

A trailing annotation per line caches the MR identity (e.g. ``PR #123``). It
is a cache only — the live forge is the source of truth — refreshed by
``anno``.

Definitions
~~~~~~~~~~~

.. list-table::
   :header-rows: 1
   :widths: 24 76

   * - Term
     - Meaning
   * - **fork-point**
     - a node with ≥ 2 children
   * - **linear node**
     - a node with ≤ 1 child (a leaf or a single-child node)
   * - **ancestors(N)**
     - root → … → parent, excluding N
   * - **subtree(N)**
     - N and all descendants, DFS pre-order
   * - **linear stack(N)**
     - ancestors(N) + N + the first-child chain down to a leaf
   * - **whole stack(N)**
     - subtree(S), where S is N's outermost ancestor short of the base branch
       (N itself when it sits on the base branch)

Scope — which branches a verb touches
-------------------------------------

``sync``, ``submit``, and ``anno`` share **one** scope computation, so they can
never disagree. Given the checked-out branch N:

.. list-table::
   :header-rows: 1
   :widths: 40 60

   * - Situation
     - Operable set
   * - N is a **fork-point** (≥ 2 children)
     - ancestors(N) + entire subtree(N) — "I manage this whole tree"
   * - N is **linear** (≤ 1 child)
     - linear stack(N) — this one line of work; sibling forks are left alone
   * - N is **not in the file**
     - just N, as a synthetic one-node stack on the base branch, with or
       without ``--all``
   * - ``--all``
     - whole stack(N), DFS pre-order — every line of N's stack; the other
       stacks on the base branch are left alone

The base branch is always removed from the operable set (it is never pushed
and never gets its own MR), but it still appears inside ``anno`` chains so
reviewers see the full lineage. A detached HEAD is an error unless a branch is
named (there is no branch to scope from), and so is standing on the base
branch, ``--all`` included: every stack shares it, so it chooses none.

Anchoring on a named branch
~~~~~~~~~~~~~~~~~~~~~~~~~~~

N above is normally the checked-out branch, but ``sync``/``submit``/``anno``
accept an optional positional branch that replaces it as the anchor — the
stack is then computed around *that* branch without checking it out. It is a
scope anchor, not a single target: an anchor mid-line still selects its
ancestors and downstream chain (the same set standing on it would), and
``--all`` widens that to its whole stack, as it would for the checked-out
branch. An explicitly named anchor must be a real branch — a live local ref,
or a name recorded in the file — so a typo fails loudly instead of quietly
resolving to an empty synthetic stack. (An
anchor that *is* a valid branch but absent from the file still becomes the
synthetic one-node stack of the next section, just as the checked-out branch
would.) A named anchor also lifts the detached-HEAD restriction, since scope
no longer depends on HEAD.

Worked examples, on the sample forest above:

* Standing on **B** (fork): operable = ``A, B, C, D, F`` (ancestors ``A`` +
  subtree of ``B``); ``main`` dropped as base.
* Standing on **D** (linear): operable = ``A, B, D, F`` (the linear stack);
  ``C`` is *not* touched — it is a sibling line.
* Standing on **D** with ``--all``: operable = ``A, B, C, D, F`` (the whole
  stack ``A`` roots); a second stack on ``main`` would be left alone.

Base resolution and per-branch base
-----------------------------------

The **base branch** is resolved once: the merge target's remote HEAD (the
``upstream`` role's holder, else ``origin``'s), then the first of
``main``/``master``/``trunk``
that exists. (The project registry is not consulted — see :doc:`stack-design`
— and there is deliberately no config key.) If nothing resolves, that is a
hard error.

A branch's **MR base** is its parent in the forest, or the base branch when the
branch is a root. This is the only place the origin/upstream distinction
reaches resolution: a root branch's MR targets the base branch on the
*merge-target* repo.

A branch absent from the file becomes a synthetic ``base → branch`` stack:
``sync``/``submit`` act on it; ``anno`` skips it (a lone MR has no neighbours
to list).

sync
----

Push every operable branch that exists locally to the ``origin`` role's remote,
with
``--force-with-lease``, in parallel. A name in the file with no local ref is a
stale entry and is skipped rather than pushed. Any push failure makes the
whole command exit non-zero (after attempting the rest). ``sync`` never
contacts the forge.

``--force-with-lease`` (not a plain force) is the deliberate choice: history
rewriting makes non-fast-forward pushes routine, but the lease still refuses
to overwrite a remote that someone else advanced.

submit
------

Reconcile the MRs to the forest. Two phases: read every branch's MR state in
parallel, then apply — base corrections fan out, creations run serially (some
forges race on duplicate detection when siblings are opened at once).
``submit`` never pushes; a branch must already be on the ``origin`` remote, or
the forge
refuses to open its MR (reported per-branch, not fatal).

Per branch, with desired base B = its parent:

1. **Open MR exists** → if its base ≠ B, retarget it to B; otherwise nothing
   to do. *Finding the MR is by its head — branch and repository — never
   filtered by base* — that is what lets a drifted base be detected at all. A
   branch with several open MRs (a platform refuses a second one only into the
   same base) keeps the one already targeting B, else retargets the most
   recently updated; the others are warned about, never closed.
2. **No open MR, a closed/merged one exists** → recreate only if our local tip
   differs from that MR's head commit, or ``--force`` is given; otherwise
   leave it (a merged branch being reused should not silently spawn a
   duplicate).
3. **No MR at all** → create it.

**Draft.** A created MR whose base is *not* the base branch starts as a draft
(a mid-stack change should not be reviewed or merged before what it sits on);
the MR that targets the base branch starts ready. ``--no-draft`` opens
everything ready. (Draft is expressed per host: GitHub a field, GitLab a
``Draft:`` title prefix, Gitea a ``WIP:`` prefix.)

**Title/body** for a new MR come from one of the branch's commits — the
latest by default, ``--title-source first`` for the oldest. Existing MRs are
never re-titled.

anno
----

Rewrite each operable MR's description with a generated navigation block,
preserving the human-written remainder. The block is one delimited region
(``<!-- wits stack: generated navigation … -->`` …
``<!-- wits stack: end navigation -->``) containing one or more
``### Stack List`` sections. Discovered MR numbers are cached back into the
machete annotations.

Block generation
~~~~~~~~~~~~~~~~

For node N, let ``prefix = ancestors(N) + [N]``, and let
``path_to_next_fork_or_leaf(start)`` walk linearly from ``start`` (following
the lone child each step) stopping inclusively at the first leaf or
fork-point.

::

   N is a fork-point (≥2 children):  one block per child Ci → prefix + path(Ci)
   N has exactly one child:          one block             → prefix + path(child)
   N is a leaf:                      one block             → prefix

A downstream walk stops at the next fork-point because that fork renders its
own multi-section description; expanding it here would grow descriptions
combinatorially.

Rendering rules
~~~~~~~~~~~~~~~

* Within a section, **only nodes that currently have an open MR are
  numbered.** An MR-less node (the base branch, or a merged middle branch) is
  not given a line, but still appears as the *parent* in its child's flow
  line, so the chain reads correctly. A root branch with no parent shows the
  base branch as its parent rather than a placeholder.
* **Only MRs in scope are looked up**, so an MR outside the scope drops out of
  the numbering the same way. A fork-point reached from one of its lines keeps
  a section for each of its other lines, but those number only the fork-point
  and its ancestors; ``--all`` brings every line in, as the rendered example
  below assumes.
* The current MR's own line is marked ``⬅️ **current**``.
* **Idempotent:** regenerating identical content replaces the old block byte
  for byte, so a second ``anno`` run reports "already up to date" and writes
  nothing. (A forge that rewrites a description's whitespace/line-endings on
  its side can defeat this and cause a harmless re-write each run.)

Block table for the sample forest
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

::

   main -> A -> B(fork) -> C(fork) -> E
                               -> G
                    -> D -> F

.. list-table::
   :header-rows: 1
   :widths: 14 86

   * - Node
     - Sections (branch names per section)
   * - ``A``
     - ``[main, A, B]`` — linear into fork B, stops at B
   * - ``B``
     - ``[main, A, B, C]`` · ``[main, A, B, D, F]`` — one per child
   * - ``C``
     - ``[main, A, B, C, E]`` · ``[main, A, B, C, G]``
   * - ``D``
     - ``[main, A, B, D, F]``
   * - ``E`` / ``F`` / ``G``
     - their own lineage to the leaf

(``main`` is the base: not annotated, shown only as a parent.)

Rendered output (B's description, B current)
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

::

   <!-- wits stack: generated navigation, do not edit below -->

   ### Stack List

     * [1/3] PR #10
       `main` ← `A`
     * [2/3] PR #11  ⬅️ **current**
       `A` ← `B`
     * [3/3] PR #12
       `B` ← `C`

   ### Stack List

     * [1/4] PR #10
       `main` ← `A`
     * [2/4] PR #11  ⬅️ **current**
       `A` ← `B`
     * [3/4] PR #13
       `B` ← `D`
     * [4/4] PR #14
       `D` ← `F`

   <!-- wits stack: end navigation -->

decorate
--------

Add labels, assignees, and reviewers to MRs — additively, and per MR.

Attributes differ from one MR to the next, so this verb is **single-MR by
default**: it acts on the named branch (or the current one). ``--all`` applies
the *same* set to every MR in that branch's whole stack — the scope ``--all``
gives the other verbs — for a uniform label like ``stacked``. Per-branch
differences are expressed by running it once per
branch with that branch's flags — typically from a per-repo script; the tool
keeps no defaults and reads no config. Flags ``--label`` / ``--assignee`` /
``--reviewer`` are each repeatable, and ``@me`` resolves to the authenticated
user; at least one is required.

Semantics:

* **Additive and idempotent.** It only adds what you list and never removes
  anything, so a project's own label/reviewer automation is never clobbered
  and re-running is safe.
* **Best-effort.** A sub-item that fails — an unknown label, a self-review the
  platform forbids — is logged and skipped; the rest still apply.
* Unlike ``anno``, it does **not** skip a standalone branch: a lone MR still
  wants labels.

Per platform, hidden behind ``apply_attributes``:

* **GitHub** uses add-only endpoints (issue labels/assignees, requested
  reviewers) — naturally additive, no read-merge.
* **GitLab** uses ``add_labels`` for labels; assignees/reviewers are id lists
  with no add verb, so it reads the current ids and unions ours in. Usernames
  (and ``@me``) resolve to numeric ids.
* **Gitea** resolves label names to ids and uses add-only label/reviewer
  endpoints; assignees are unioned through an issue edit.

slice
-----

Cut the commits on top of a base into named branches. ``slice`` runs ``git
rebase -i --update-refs`` and edits the todo git generated rather than writing
its own, so the todo holds what a plain ``git rebase -i`` would under your
config — fixups folded into their targets when ``rebase.autoSquash`` is set,
commits already upstream left out — plus the ``update-ref`` lines naming the
branches. The refs move at the *end* of the rebase. The branch list is read
back from the saved todo, not from a post-rebase ``base..HEAD`` scan (a branch
whose line you removed can still point into the range).

What each ``update-ref`` line is, per position
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

A *position* is the point after a ``pick`` and the fixups folded into it. The
line there is chosen from what we know, so re-slicing an existing stack needs
no retyping:

1. **A branch already in the stack** ends here → the line is **active**
   (uncommented) under that real name, so the branch is preserved in place.
2. **No stack branch, but some branch** ends here → a **commented** line with
   that branch name (a suggestion to adopt it).
3. **No branch at all** → a **commented** ``<prefix><slug>`` suggestion — the
   name to mint for fresh work.

Where an existing branch ends is git's call (``--update-refs``): after the
fixups folded into its commit, and for a branch sitting on a fixup commit,
where that commit stood before it was moved. The **checked-out branch** always
ends at the last position, because git moves it there itself: its line is
added at the end, it must stay after the last commit (``slice`` refuses a todo
that moves it up), and it is kept out of what git executes.

At most **one** line per position is ever active. Several branches at one
position are not a fork (a fork diverges later); activating two would make the
linear record collapse them into a bogus parent→child chain (an empty MR), so
the extras are demoted to commented suggestions. The names you uncomment are
de-duplicated and the base is dropped before writing.

* **A single slice is linear by nature** — a rebase range is one line of
  commits, so ``update-ref`` can only mark points along it. Forks are not
  expressible in one slice.
* **Before writing**, the assignment list is de-duplicated and the base itself
  is dropped, so a slug collision (two similar commit subjects suggesting the
  same name) or a stray ``update-ref`` to the base cannot create a self-loop.
* **Writing** lays the branches as a chain ``base → b1 → … → bn`` via
  ``reparent``, which **refuses any link that would form a cycle** and leaves
  unrelated stacks in the file untouched.
* **A stack branch checked out in another worktree** stops the slice before
  the rebase starts: git will not move a branch another worktree has checked
  out, so the chain could only be recorded without it.

Growing or rebuilding an existing stack
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

``slice`` records the branches whose ``update-ref`` line is active and chains
them from ``base``. Because branches already in the stack come pre-filled
active (tier 1 above), re-slicing preserves them automatically — you only
touch the lines you want to change.

* **Append to the tip.** Run ``slice --base <tip-branch>`` so the todo holds
  only the new commits; uncomment a name for each and they attach under the
  tip, the existing chain untouched (slicing a sub-range never disturbs the
  rest of the forest). With the default base instead, the existing branches
  are already active, so you would just uncomment the new tail.
* **Insert or rebuild within a line.** Slice from the line's base: the
  existing branches are already active in commit order, so you only uncomment
  the new middle one (and reorder commits as needed). ``reparent`` *moves*
  the downstream branch under the new node (detaching it from its old
  parent), so the result is a clean chain with no orphan — unlike removal,
  insertion leaves nothing behind.

Editing the structure (``wits stack tree``)
-------------------------------------------

``slice`` builds the forest from a rebase; ``tree`` is the set of direct edits
for everything else. The rule shared by all of them: **removing a branch never
discards the work above it** — ``remove`` splices a node's children up into
its slot, so a mid-stack deletion keeps the downstream line (and ``submit``
then retargets its base). The base branch is protected from removal.

.. list-table::
   :header-rows: 1
   :widths: 26 74

   * - Command
     - Effect
   * - ``tree prune``
     - Drop every node whose branch no longer exists locally; each removed
       node's children splice up. Needs no names, idempotent, network-free —
       the automation cleanup. A live fork sibling keeps its node (its ref
       still exists), so it is never collateral.
   * - ``tree rm <branch>… [--delete]``
     - Remove named branches from the stack (children splice up).
       ``--delete`` also runs ``git branch -d`` (``-D`` with ``--force``).
       Refuses to remove the base branch.
   * - ``tree mv <branch> --onto <parent>``
     - Re-parent a branch; **its whole substack moves with it** (children
       come along). Refused if it would place the branch beneath its own
       descendant (the cycle guard). Creates the node if it was not recorded
       yet, so ``mv`` also serves as "add this branch onto X".
   * - ``tree rename <from> <to>``
     - Follow a ``git branch -m``: the node keeps its parent, its slot among
       that parent's siblings, its substack and its MR annotation — only the
       name changes. ``<to>`` must be an existing local branch. A ``<from>``
       that was never stacked is reported and ignored; a ``<to>`` already in
       the forest is refused (it can only be a stale entry, since git will not
       rename onto a live branch — ``tree prune`` is the way out). Unlike
       ``rm`` and ``mv``, the base branch is **not** protected.

``tree mv`` changes the *declared* shape only — it does not move commits.
After a move, rebase the branch onto its new parent for the code to match;
``submit`` then retargets the MR base.

``tree rename`` is likewise a file edit and never renames a git branch: the
git rename has already happened, which is why ``<to>`` must exist. It is not
``rm`` + ``mv``, because that sequence would lose the MR annotation, append the
node after its former siblings instead of restoring its slot, and reorder the
file — three silent losses for an operation in which nothing about the stack
changed except a name.

Adding and removing mid-stack
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

The verbs are stateless re-readers of the file, so dynamic correctness is
just a matter of keeping the file in step with reality; ``submit``'s base
correction does the rest.

.. list-table::
   :header-rows: 1
   :widths: 40 60

   * - Operation
     - How / Result
   * - **Insert** B between A and C
     - ``slice`` (re-run), ``tree mv B --onto A`` then ``tree mv C --onto B``,
       or edit the file. C ends up under B; ``submit`` creates B's MR and
       retargets C's base to B.
   * - **Remove** B (between A and C)
     - ``tree rm B``, ``tree prune`` (after deleting B's branch), or delete
       B's line in the file. C reattaches to A; ``submit`` retargets C's base
       to A.
   * - **Remove** B via re-``slice``
     - Re-run ``slice``, assign A, C. ⚠️ ``slice`` does not prune, so B
       lingers as a dead sibling. Follow with ``tree prune`` (if B's branch
       is gone) or ``tree rm B``.

Deleting a branch from the file by hand is still valid — the parser is
indentation-agnostic, so the orphaned child reattaches to the grandparent
without re-indenting.

Known limitations
-----------------

* **Re-``slice`` does not prune** a branch dropped from a line; it lingers as
  a dead node (auto-pruning inside ``slice`` is unsafe — it cannot be told
  apart from a fork sibling that should survive). Clean it with ``tree prune``
  (once the branch is deleted) or ``tree rm``.
* **A stack branch mid-rebase in another worktree** slips past the worktree
  check, since that worktree reports a detached HEAD. Git still leaves the
  branch unmoved, and ``slice`` records the chain without it, as if its line
  had been removed.
* **An MR orphaned by removal** (its branch no longer in the file) keeps its
  old navigation block; ``anno`` no longer touches it.
* **``tree mv`` is metadata only** — it does not rebase commits; you must
  restack the branch yourself for the MR to be meaningful.
* **Cross-fork MRs** are supported on all three: GitHub/Gitea via the
  ``owner:branch`` head; GitLab via its cross-project mechanism (create on
  the source project with a numeric ``target_project_id``, read/edit on the
  target), which costs two extra project-id lookups when a fork is detected.
* **Gitea/Forgejo base changes** depend on the server version honouring the
  ``base`` field; a server that doesn't degrades to a per-branch warning,
  never a corruption.
* **On Gitea, and Forgejo before 16, finding a branch's MR reads the
  repository's whole PR list**: those servers cannot filter PRs by head
  branch, so the answer stays complete only by scanning. Forgejo 16 and later
  (Codeberg included) filter on the server and answer in one request.

Where the logic lives
---------------------

.. list-table::
   :header-rows: 1
   :widths: 44 56

   * - Concept
     - Location
   * - forest parse/serialize, tree algebra
     - ``crates/wits/src/cmd/stack/topology.rs`` (``ancestors``, ``subtree``,
       ``linear_stack``, ``whole_stack``, ``anno_blocks``, ``reparent``)
   * - scope selection, base resolution, per-branch base
     - ``crates/wits/src/cmd/stack/resolution.rs``
   * - push
     - ``crates/wits/src/cmd/stack/sync.rs``
   * - MR reconcile decision
     - ``crates/wits/src/cmd/stack/submit.rs`` (``decide``)
   * - navigation rendering + splice
     - ``crates/wits/src/cmd/stack/anno.rs``
   * - attribute application (labels/assignees/reviewers)
     - ``crates/wits/src/cmd/stack/decorate.rs`` and
       ``crates/wits-util/src/forge/*`` (``apply_attributes``)
   * - structure edits (``prune``/``rm``/``mv``), ``remove`` splice
     - ``crates/wits/src/cmd/stack/tree.rs`` and
       ``crates/wits/src/cmd/stack/topology.rs`` (``remove``)
   * - forge primitives + normalized MR + detection
     - ``crates/wits-util/src/forge/``
   * - the role vocabulary + which remote holds each
     - ``crates/wits-util/src/remote.rs``
   * - resolving roles from a declaration, and routing per checkout
     - ``crates/wits-util/src/project/remotes.rs``
   * - remote URL parsing + the identities behind the roles
     - ``crates/wits-util/src/forge/remote.rs``

Invariants
----------

1. ``sync``, ``submit``, ``anno`` (and ``decorate --all``) must share one
   scope computation — never fork the fork-point rule across verbs.
2. ``anno_blocks`` must stop at the next fork-point; expanding it grows
   descriptions combinatorially.
3. Exactly one navigation marker pair per description; stripping relies on
   it.
4. The base branch is excluded from the operable set but included in
   ``anno`` chains.
5. An MR lookup matches the head — branch *and* repository, by identity —
   never the base, and reads every page. Matching the base hides a drifted
   MR, matching the branch name alone takes another fork's MR for ours, and a
   partial list hides one that exists; each ends with ``submit`` acting on the
   wrong MR or opening a duplicate.
6. ``reparent`` must refuse cycles; it is the only operation that can
   introduce one (parsing yields a forest by construction).
7. ``remove`` must splice children up, never drop the subtree; removing a
   branch cannot destroy the work stacked above it.
8. ``decorate`` is additive only — it adds attributes and never removes
   them, so it cannot clobber a project's own label/reviewer automation.
