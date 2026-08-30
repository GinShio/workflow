.. _review-store:

``wits review`` — the store and how to move it
==============================================

``wits review`` keeps its state in JSON files and a few git refs. This is the
reference for that layout: what lives where, and how to carry an in-progress
review to another machine.

Where the store lives
---------------------

The base directory is resolved on a three-rung ladder, first hit wins:

.. list-table::
   :header-rows: 1
   :widths: 14 86

   * - Rung
     - Path
   * - 1
     - ``$WITS_REVIEW_DIR`` — an explicit override.
   * - 2
     - ``$XDG_STATE_HOME/wits/review`` — when ``XDG_STATE_HOME`` is set.
   * - 3
     - ``<common-git-dir>/wits/review`` — the default, per-clone, beside the
       machete file.

Rung 3 uses the **common** git directory (``git rev-parse --git-common-dir``),
not the per-worktree one, so a ``checkout`` worktree and the main clone resolve
to the *same* store — you can review from either. (The snapshot pins in
``refs/wits/review/*`` already live in the shared ref store, so they were
always worktree-safe.)

State (this store, ``$XDG_STATE_HOME``) is kept separate from config (the feed
``review.toml``, ``$XDG_CONFIG_HOME``). Under the base, each repo has its own
subtree keyed by the target remote's identity, and each MR has its own
directory::

   <base>/<host>/<owner>/<repo>/
   ├── <id>/
   │   ├── info.json       # the MR's metadata + diff state
   │   ├── comments.json   # the forge's discussion (a cache)
   │   ├── local.json      # your unsubmitted review (only present while drafting)
   │   └── inflight.json   # deferred cleanup ids (only after a failed submit)
   ├── <id>/ …
   └── current             # the MR the last `checkout` materialized

For a GitLab nested group the ``<owner>`` segment contains slashes and becomes
nested directories — that is fine.

The files
---------

``info.json`` — metadata + snapshot history
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

The MR's necessary information: the inbox row, the detail header, and the
history of review points. A **pure cache** — ``fetch`` regenerates it, so it
is not meant to be hand-edited (edits are overwritten on the next fetch).

.. list-table::
   :header-rows: 1
   :widths: 22 78

   * - Field
     - Meaning
   * - ``schema``
     - Store version.
   * - ``mr``
     - The MR object (id, display, state, draft, title, author, base, source,
       head_sha, updated_at, labels, web_url — see
       :doc:`review-json`).
   * - ``snapshots``
     - The review points fetched so far, oldest first: each a
       ``{ fork_sha, start_sha, head_sha }``. The last is the current one.
       Every snapshot's objects are pinned.
   * - ``fetched_at``
     - Unix seconds of the **last** ``fetch`` that synced this MR — updated on
       every fetch (even for an unchanged head), so dormancy tracks real
       staleness. ``0`` for a feed-only entry (never fully fetched).
   * - ``commits``
     - Commits in the current snapshot's ``fork..head``, derived **locally**
       from the fetched objects.
   * - ``files``
     - Files the current snapshot touched, derived locally.

Three SHAs describe a review point:

.. list-table::
   :header-rows: 1
   :widths: 22 78

   * - Field
     - Meaning
   * - ``fork_sha``
     - ``merge-base(target, head)`` — where the series left its target,
       computed at fetch. **The diff endpoint**, and what GitLab's comment
       ``position`` wants for its ``base_sha``, those being the same commit on
       that forge.
   * - ``start_sha``
     - GitLab's separate diff-version start. A copy of the fork on GitHub,
       which has no such notion.
   * - ``head_sha``
     - The reviewed tip.

**The forge's own ``base`` is deliberately not stored.** The two forges do not
agree on what it means — GitLab's ``diff_refs.base_sha`` is already the merge
base, GitHub's ``baseRefOid`` is the target branch's *current tip* — so it is
an input to a fetch, not a property of a review point. Keeping it invited
exactly one bug: a value that is not an ancestor of ``head``, quietly serving
as a diff endpoint and widening every comparison built on it (22 files
reported against 4 real ones, on the MR where this was found).

**A fork point must be an ancestor of its own head.** That is what makes
``fork..head`` a patch series rather than a two-endpoint tree compare, and
``fetch`` now enforces it: it acquires the target's objects properly — the
bare commit first, then the target branch by name — and **fails** rather than
recording a snapshot whose fork it could not resolve. A record from before
that rule can only be repaired by re-fetching, so the read path checks the
ancestry and says so.

A **feed** fetch fills only ``mr`` (and stamps ``fetched_at``), leaving
``snapshots``/``commits``/``files`` empty; a full ``fetch <mr>`` fills them
and appends a snapshot when the head has moved. A **snapshot** (a stored,
pinned review point) is distinct from a diff **range** (a throwaway query).
``prune --older-than`` reads the MR-level ``fetched_at``, not a per-snapshot
time.

``comments.json`` — the forge's discussion (a cache)
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

``{ "schema": 1, "threads": [ … ] }``, where each thread has the shape in
:doc:`review-json`. This is a pure cache: overwrite or delete it freely and
refetch. Everyone else's comments live here.

``local.json`` — your unsubmitted review (the file you edit)
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

The one file you write, defined in :doc:`review-json`: an optional ``verdict``
and an append-style ``actions`` list. The review summary is a ``summary``
action, not a top-level field. Every stored action has an id; later actions
with the same id replace earlier ones, and ``drop`` removes a live local
action. It exists only while you have a draft — ``submit`` deletes it once
flushed, and an empty draft is the same as no file. This is the state that
would be *lost*, so it is what migration moves.

You need not know this path to write it: ``wits review draft <mr> -`` (or a
file) hands a batch of actions to the tool, which appends, assigns missing
ids, and validates them. Editing the file directly is equivalent; the tool
reading it does not care who wrote it. ``wits review draft <mr> --dedup``
compacts the append-only stream in place.

``inflight.json`` — deferred forge-side cleanup (transient)
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

The one file a *failed* ``submit`` leaves behind: the forge-side ids it
created but could not publish (a GitHub pending review, GitLab draft notes).
The next ``submit`` deletes them **first** — touching only ids wits itself
recorded — and removes the file once the cleanup is done, so an orphan can
never be published twice. ``submit --all`` counts it as pending work, so a
deferred cleanup is retried even after the draft that spawned it is gone.

Git refs — pinning reviewed objects
-----------------------------------

Fetching an MR pulls its objects and holds them alive with the tool's own
refs, so a later force-push on the author's side cannot garbage-collect the
snapshot you reviewed:

.. list-table::
   :header-rows: 1
   :widths: 44 56

   * - Ref
     - Points at
   * - ``refs/wits/review/<mr>/<snapshot-sha>``
     - the reviewed head commit
   * - ``refs/wits/review/<mr>/<snapshot-sha>-base``
     - where the fork computation pins the base commit, created when the
       base's objects had to be fetched to compute it

The names carry only what disambiguates within one clone — the MR number and
the SHA. Enumerate them with ``git for-each-ref refs/wits/review/``; ``prune``
deletes them (letting git collect the objects) once an MR is terminal or
dormant.

Moving a review to another machine
----------------------------------

"Sharing" here means carrying *your own* in-progress review between *your own*
machines — the forge is the collaboration layer, not this store. Because
``info.json``/``comments.json`` are refetchable, only ``local.json`` needs to
travel:

.. code-block:: sh

   # on the first machine — copy the drafts you care about
   base=$(git rev-parse --path-format=absolute --git-common-dir)/wits/review/github.com/me/proj
   cp "$base/123/local.json" /media/key/mr123-local.json

   # on the second machine
   base=$(git rev-parse --path-format=absolute --git-common-dir)/wits/review/github.com/me/proj
   mkdir -p "$base/123" && cp /media/key/mr123-local.json "$base/123/local.json"
   wits review fetch 123        # rebuild info/comments and pin the objects
   wits review show 123         # your pending review is back, merged in

``local.json`` refers to threads and lines on the MR's current snapshot, so
the second machine must be able to ``fetch`` the same MR — it can, the MR
still exists on the forge. If you point ``WITS_REVIEW_DIR`` at synced storage,
both machines share the drafts automatically and only the per-clone git refs
are rebuilt by ``fetch``.

Schema versioning
-----------------

Every file and every ``--json`` payload carries an integer ``schema``
(currently ``1``); an incompatible shape change bumps it. Because
``info.json``/``comments.json`` are disposable, a bump can be handled for
them by refetching; only ``local.json`` migrations (if ever needed) warrant
more care.
