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

The stack forest
----------------

Each stack branch records, in its own ``branch.<name>`` git config section,
the branch it sits on, its place among its siblings and its cached MR — one
stack per repository, shared by all its worktrees. Together they make a
dependency **forest**, shown and edited (``tree edit``) as git-machete's text:
each line is a branch, and indentation means "sits on top of". It is a forest,
not just a chain — a branch may fork into several::

   main
       A            single child: linear
           B        fork-point (children C, D)
               C    leaf
               D    single child: linear
                   F  leaf

Two properties of the text form matter:

* **Reading is indentation-agnostic.** Only *relative* nesting is read: a
  line's parent is the nearest preceding line with strictly smaller indent.
  Two spaces, four, or a tab all parse identically, and deleting a middle line
  leaves its child correctly attached to the grandparent even if the child is
  not re-indented.
* **Rendering normalizes to four spaces**, regardless of the width that was
  read.

Git keeps the forest honest across branch events: ``git branch -m`` moves a
branch's section, so it keeps its place, substack and cached MR; ``git branch
-d``/``-D`` deletes it, so the branch leaves the stack. A child whose recorded
parent is gone — renamed or deleted — is placed by history: under the deepest
stack branch with an intact place whose tip is an ancestor of the child's, else
on the base branch. That finds a renamed parent and splices a deleted one's
children up to its parent; the next command that edits the stack writes the
placement down.

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

``push``, ``submit``, and ``anno`` share **one** scope computation, so they can
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
   * - N is **not in the stack**
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

N above is normally the checked-out branch, but ``push``/``submit``/``anno``
accept an optional positional branch that replaces it as the anchor — the
stack is then computed around *that* branch without checking it out. It is a
scope anchor, not a single target: an anchor mid-line still selects its
ancestors and downstream chain (the same set standing on it would), and
``--all`` widens that to its whole stack, as it would for the checked-out
branch. An explicitly named anchor must be a local branch, so a typo fails
loudly instead of quietly resolving to an empty synthetic stack. (An anchor
that *is* a branch but not in the stack still becomes the synthetic one-node
stack of the next section, just as the checked-out branch would.) A named anchor also lifts the detached-HEAD restriction, since scope
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

The **base branch** is resolved once, as the checkout's trunk: the
``main_branch`` the owning project declares, else the merge target's remote
HEAD (the ``upstream`` role's holder, else ``origin``'s), else the first of
``main``/``master``/``trunk`` that exists. (The rule is shared with
``worktree`` and the git hooks — see :doc:`stack-design` — and there is
deliberately no config key.) If nothing resolves, that is a hard error.

A branch's **MR base** is its parent in the forest, or the base branch when the
branch is a root. This is the only place the origin/upstream distinction
reaches resolution: a root branch's MR targets the base branch on the
*merge-target* repo.

A branch not in the stack becomes a synthetic ``base → branch`` stack:
``push``/``submit`` act on it; ``anno`` skips it (a lone MR has no neighbours
to list).

push
----

Push every operable branch that exists locally to the ``origin`` role's remote,
with
``--force-with-lease``, in parallel. A root of the forest with no local ref is
skipped rather than pushed. Any push failure makes the
whole command exit non-zero (after attempting the rest). ``push`` never
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

publish
-------

``push``, then ``submit``, then ``anno``, over one plan. Each branch's MRs are
looked up once, by ``submit``'s phase, and the MRs it leaves open — found,
retargeted or created — are what ``anno``'s phase numbers and annotates; a
created MR under ``-n`` does not exist, so it is neither numbered nor annotated.
A branch whose push failed is looked up but not acted on: its open MR, if any,
still appears in the navigation, and no MR is created or retargeted for it.
Takes ``submit``'s flags and scope; a standalone branch is pushed and submitted
and, like under ``anno``, gets no navigation. Exits non-zero when any step
failed for any branch.

anno
----

Keep one navigation comment on each operable MR, written by the token's user.
Its body is one delimited region (``<!-- wits stack: generated navigation …
-->`` … ``<!-- wits stack: end navigation -->``) containing one or more
``### Stack List`` sections. Discovered MR numbers are cached in each branch's
``witsMr``.

The comment
~~~~~~~~~~~

Per MR, ``anno`` lists the conversation comments — every page — and takes as
candidates the ones the token's user wrote whose body *starts* with the
opening marker (one that merely quotes it is a reply, not the navigation):

1. **No candidate** → post the navigation as a new comment.
2. **Candidates** → the oldest is the navigation comment: edit it if its text
   differs (line endings and outer whitespace aside), otherwise leave it.
   Further candidates are warned about with their links and left in place.
3. A navigation comment written by **another account** (an earlier token's
   user, say) is never edited; it is warned about, and the user's own comment
   is created beside it.
4. **Migration:** if the MR's description still holds the block older
   versions wrote there, it is stripped from the description — the rest of the
   text kept, a torn footer recovered — *after* step 1 or 2 succeeded, so a
   failure part-way never leaves the MR without navigation.

Nothing on the forge is ever deleted, pinned, or posted beyond that one
comment. A failed listing fails that MR, never counting as "no comment yet",
and a failed create is not retried, since it may have landed; either way the
next run sees the truth. Each MR is independent: a failure is warned about and
counted, and the others proceed. ``--dry-run`` prints one line per action:
``create <noun> <display> navigation comment (<branch>)``, ``update …`` or
``strip navigation from <noun> <display> description (<branch>)``.

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
own multi-section navigation; expanding it here would grow navigation comments
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
* **Idempotent:** a comment whose text already matches is not edited — line
  endings and outer whitespace a forge rewrites do not count as a change — so a
  second ``anno`` run reports "navigation already up to date" and writes
  nothing.

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

Rendered output (B's navigation comment, B current)
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

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
  platform forbids — is logged and skipped; the rest still apply. So is one
  the platform accepts and then drops without an error, where its answer
  shows it: an assignee past the first on a GitLab tier that allows one, an
  assignee set without write access on Gitea.
* **Labels are added, never created.** A label must already exist (on GitHub
  in the repository; on GitLab in the project or a group above it; on Gitea
  in the repository or its organisation); a missing one is warned about.
* Unlike ``anno``, it does **not** skip a standalone branch: a lone MR still
  wants labels.

Per platform, hidden behind ``apply_attributes``:

* **GitHub** looks each label up by name and uses add-only mutations (labels,
  assignees, requested reviewers) — naturally additive, no read-merge.
* **GitLab** checks each label exists before ``add_labels``, which would
  otherwise create it; assignees/reviewers are id lists with no add verb, so
  it reads the current ids, unions ours in, and checks the answer for any
  GitLab dropped. Usernames (and ``@me``) resolve to numeric ids.
* **Gitea** adds labels by name (Gitea 1.22, Forgejo 8; an organisation's
  labels from Gitea 1.23, Forgejo 10) and checks the answer for any it did
  not know; reviewers are requested one at a time, since the server stops at
  the first it refuses; assignees are unioned through an issue edit and
  checked in its answer. An exclusive scoped label still displaces the other
  labels of its scope — Gitea's own rule.

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
  unrelated stacks untouched.
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
   * - ``tree rm <branch>… [--delete]``
     - Remove named branches from the stack (children splice up).
       ``--delete`` also runs ``git branch -d`` (``-D`` with ``--force``).
       Refuses to remove the base branch.
   * - ``tree mv <branch> --onto <parent>``
     - Re-parent a branch; **its whole substack moves with it** (children
       come along). Refused if it would place the branch beneath its own
       descendant (the cycle guard). Creates the node if it was not recorded
       yet, so ``mv`` also serves as "add this branch onto X".
   * - ``tree edit [FILE|-]``
     - Rewrite the whole forest as text, in git's editor, or from a file
       (``-`` for stdin). Writes only what changed; refuses a name that is
       neither a local branch nor the base, a name given twice, and the base
       indented under another branch — changing nothing in each case.

``tree mv`` changes the *declared* shape only — it does not move commits.
After a move, rebase the branch onto its new parent for the code to match;
``submit`` then retargets the MR base.

Adding and removing mid-stack
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

The verbs are stateless re-readers of the stack, so dynamic correctness is
just a matter of keeping it in step with reality; ``submit``'s base correction
does the rest.

.. list-table::
   :header-rows: 1
   :widths: 40 60

   * - Operation
     - How / Result
   * - **Insert** B between A and C
     - ``slice`` (re-run), ``tree mv B --onto A`` then ``tree mv C --onto B``,
       or ``tree edit``. C ends up under B; ``submit`` creates B's MR and
       retargets C's base to B.
   * - **Remove** B (between A and C)
     - ``tree rm B``, ``git branch -D B``, or delete B's line in ``tree
       edit``. C reattaches to A; ``submit`` retargets C's base to A.
   * - **Remove** B via re-``slice``
     - Re-run ``slice``, assign A, C. ⚠️ ``slice`` does not prune, so B
       lingers as a dead sibling. Follow with ``tree rm B``, or delete B's
       branch.

Deleting a line in ``tree edit`` is valid — the parser is indentation-agnostic,
so the orphaned child reattaches to the grandparent without re-indenting.

Known limitations
-----------------

* **Re-``slice`` does not prune** a branch dropped from a line; it lingers as
  a dead node (auto-pruning inside ``slice`` is unsafe — it cannot be told
  apart from a fork sibling that should survive). Clean it with ``tree rm``,
  or by deleting the branch.
* **A stack branch mid-rebase in another worktree** slips past the worktree
  check, since that worktree reports a detached HEAD. Git still leaves the
  branch unmoved, and ``slice`` records the chain without it, as if its line
  had been removed.
* **An MR orphaned by removal** (its branch no longer in the stack) keeps its
  last navigation comment; ``anno`` no longer touches it.
* **A navigation comment keeps the place it was posted at.** Run ``anno``
  right after ``submit`` and it sits directly under the description; an MR
  that already has a discussion when it is first annotated (one migrated from
  the description, say) gets it at the end of that discussion.
* **``tree mv`` is metadata only** — it does not rebase commits; you must
  restack the branch yourself for the MR to be meaningful.
* **Cross-fork MRs** are supported on all three: GitHub names the head
  repository by id (``headRepositoryId``); Gitea via the ``owner:branch``
  head; GitLab via its cross-project mechanism (create on the source project
  with a numeric ``target_project_id``, read/edit on the target), which costs
  two project-id lookups the first time a stack needs them. A fork is any
  push repository other than the target, so a fork the target's own
  organisation holds counts too. Gitea names that one ``owner/repo:branch``,
  which needs Gitea 1.26 or later; Forgejo parses no such head and refuses
  it.
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
     - ``crates/wits/src/cmd/stack/push.rs``
   * - MR reconcile decision
     - ``crates/wits/src/cmd/stack/submit.rs`` (``decide``)
   * - navigation rendering, the navigation comment, description migration
     - ``crates/wits/src/cmd/stack/anno.rs``
   * - attribute application (labels/assignees/reviewers)
     - ``crates/wits/src/cmd/stack/decorate.rs`` and
       ``crates/wits-util/src/forge/*`` (``apply_attributes``)
   * - structure edits (``rm``/``mv``/``edit``), ``remove`` splice
     - ``crates/wits/src/cmd/stack/tree.rs`` and
       ``crates/wits/src/cmd/stack/topology.rs`` (``remove``)
   * - where the forest is stored, placement by history
     - ``crates/wits/src/cmd/stack/store.rs``
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

1. ``push``, ``submit``, ``anno`` (and ``decorate --all``) must share one
   scope computation — never fork the fork-point rule across verbs.
2. ``anno_blocks`` must stop at the next fork-point; expanding it grows
   navigation comments combinatorially.
3. Each MR carries one wits navigation comment, written by the token's user
   and recognised by the marker its body starts with; ``anno`` keeps the
   oldest current, never edits another account's, and deletes none. After the
   first ``anno`` the description carries no navigation. The marker never
   changes, or every comment already posted would be orphaned.
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
