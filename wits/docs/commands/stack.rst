.. _wits-stack:

``wits stack``
==============

Turn a chain of local branches into a set of merge requests that reviewers can
actually navigate — and keep them in sync as you reshape the stack. You do the
local work however you like (``git rebase``, ``git-branchless``, plain
commits); ``wits stack`` handles the remote half: pushing the branches, opening
an MR for each against the right base, and keeping a navigation comment on
every MR so a reviewer can walk the whole stack.

The mental model
----------------

Three facts make up the state of a stack on the remote, and there is one verb
for each. They are independent on purpose — run any one of them on its own, and
re-run it freely; each only reconciles its own slice of the world:

.. list-table::
   :header-rows: 1
   :widths: 24 30 46

   * - Verb
     - Owns
     - Touches
   * - ``wits stack sync``
     - branch **content** on the remote
     - git only (push)
   * - ``wits stack submit``
     - MR **existence** and **base**
     - the forge API
   * - ``wits stack anno``
     - each MR's **navigation comment**
     - the forge API

Plus a few helpers: ``wits stack slice`` cuts commits into the stack in the
first place, ``wits stack decorate`` adds labels / reviewers / assignees to an
MR, and ``wits stack tree`` edits the stack's structure.

The dependency tree itself lives in the **machete file** (the same format
``git-machete`` uses): one branch per line, indentation meaning *sits on top
of*. It sits at ``<common-git-dir>/machete`` — one forest per *repository*, so
every worktree of it reads and writes the same stack rather than each keeping
a private one that ``git worktree remove`` would take with it::

   main
       feature-api
           feature-ui
       feature-docs

Here ``feature-api`` and ``feature-docs`` both build on ``main``;
``feature-ui`` builds on ``feature-api``. You do not have to hand-write this
file — ``slice`` generates it — but it is plain text and safe to edit.

Beside it sits an empty ``machete.lock``, which every edit ``flock``\ s so that
two of them — yours and the ``reference-transaction`` hook's, say — take turns.
It stays there on purpose: unlike git's ``*.lock`` files its existence means
nothing, and deleting it while an edit is waiting is the one way to let two
edits overlap.

.. note::

   The file is a *forest*, not just a chain — a branch may fork into several.
   The precise rules — how scope is chosen on a fork, how forks render, what
   happens when you add or remove a branch mid-stack — live in
   :doc:`/reference/stack-behavior`. This guide stays at the
   getting-things-done level.

One-time setup
--------------

**A token for your forge.** Opening and editing MRs needs an API token. Put it
in git config (per host is the most precise; a blanket key works too):

.. code-block:: sh

   git config wits.forge.github.com.token  ghp_xxx
   # or, less specific:
   git config wits.forge.github.token       ghp_xxx
   git config wits.forge.token              ghp_xxx

Or supply it through the environment, which always works and is handy on CI:

.. code-block:: sh

   export GITHUB_TOKEN=ghp_xxx     # GITLAB_TOKEN / GITEA_TOKEN / FORGEJO_TOKEN / CODEBERG_TOKEN

``sync`` needs no token (it only pushes); ``submit``, ``anno``, and
``decorate`` do.

**Remotes: the ``origin`` and ``upstream`` roles.** ``wits stack`` reads two
remotes by role:

* **``origin``** — where it pushes, and the source side of every MR. You need
  push rights here.
* **``upstream``** — the repository MRs merge *into*. Set this when you work on
  a fork; leave it unset when you push and merge in the same repo (then the
  ``origin`` holder plays both parts).

In a repository ``wits project`` does not know about, the roles come straight
from the remote **names** — so the conventional setup needs no configuration at
all:

.. code-block:: sh

   git remote add origin   git@github.com:me/project.git
   git remote add upstream git@github.com:acme/project.git   # only if you forked

In a declared project the roles come from its config file instead, which is
what lets the remotes be named anything you like (see
:ref:`project-reference`). Either way ``stack`` asks the same question and
never needs telling twice.

The forge (GitHub / GitLab / Gitea / Forgejo / Codeberg) is detected from the
**upstream** role's URL — or the ``origin`` holder's when nothing holds
``upstream``. A self-hosted instance behind a custom domain can be named
explicitly:

.. code-block:: sh

   git config wits.forge.git.acme.com.service gitlab
   git config wits.forge.git.acme.com.api-url https://git.acme.com/api/v4

Building a stack with ``slice``
-------------------------------

``slice`` cuts the commits sitting on top of your base branch into named
branches. It opens the interactive rebase todo git itself would give you —
fixups already folded into their targets if you set ``rebase.autoSquash`` —
with a commented branch suggestion after each commit:

.. code-block:: sh

   wits stack slice              # slices <base>..HEAD
   wits stack slice --base main

::

   pick a1b2c3d # Add the API layer
   fixup 9f8e7d6 # fixup! Add the API layer
   # update-ref refs/heads/me/add-the-api-layer

   pick d4e5f6a # Wire up the UI
   # update-ref refs/heads/work

Uncomment the ``update-ref`` line after each commit a branch should end on,
save, and let the rebase finish. The branches move at the end of the rebase,
and the machete file is written to match. The last line names the branch you
are on (``work`` here): uncomment it to keep that branch as the top of the
stack, and leave it last — git moves the checked-out branch to the end of the
rebase itself. Branch-name suggestions use ``wits.stack.prefix`` if set,
otherwise a slug of your ``user.name``, otherwise ``stack/``.

You do not *have* to use ``slice`` — any branches you create yourself and
record in the machete file work identically. And a branch that is not in the
file at all is treated as a one-branch stack (see `Single branches`_).

Growing or reshaping an existing stack
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

When you re-run ``slice``, branches already in the stack come **pre-filled** in
the todo (active, under their real names), so re-slicing preserves them — you
only edit the lines you want to change.

.. code-block:: sh

   # Append: you've added commits on top of a finished stack. Slice from the tip,
   # so only the new commits are in play; uncomment a name for each.
   wits stack slice --base feature-c

   # Insert/rebuild: slice from the line's base. The existing branches are already
   # active in order — just uncomment the new middle branch (and reorder commits as
   # needed). The downstream branch is moved under it automatically, no leftovers.
   wits stack slice

The everyday loop
-----------------

After reworking your commits:

.. code-block:: sh

   wits stack sync       # push every branch in the stack to the origin remote
   wits stack submit     # open MRs that don't exist; fix bases that moved
   wits stack anno       # keep each MR's navigation comment current

Run them in that order the first time; afterwards run whichever matches what
changed. Reordered the stack but did not touch code? ``submit`` alone fixes the
MR bases. Just amended a commit? ``sync`` alone re-pushes.

Where the navigation lives
~~~~~~~~~~~~~~~~~~~~~~~~~~

The navigation is one comment on each MR, written by you (the token's user)
and edited in place — never part of the description. In a repository that
squash-merges, the description becomes the landed commit's message, while a
comment is part of no commit whatever the merge method.

A comment keeps the place in the conversation it was posted at, so run
``anno`` right after ``submit``, as above: the navigation then sits directly
under the description, ahead of anyone else's comments. ``anno`` only edits its
own comment, never anyone else's, and deletes nothing; if a second navigation
comment of yours ever appears (two runs at once, say), it keeps the oldest
current and tells you where the other one is.

An MR whose description still holds the block an older ``wits`` wrote there is
migrated by the next ``anno``: the comment is written first, then the block is
stripped from the description, the rest of your text untouched.

Scope — which branches each verb touches
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

By default a verb acts on the stack around the branch you are standing on, with
one rule that is worth knowing:

* **On a linear branch** (zero or one child): it acts on *that line of work* —
  your ancestors, you, and the primary downstream chain. Sibling branches that
  fork off elsewhere are left alone.
* **On a fork-point** (two or more children): it acts on the *whole tree* you
  are the root of — every branch below you, plus your ancestors.

Pass ``--all`` to act on the *whole stack* you are in — every line of it, from
the branch sitting on the base branch upward — whichever of its branches you
are standing on. Other stacks on the same base branch are left alone:

.. code-block:: sh

   wits stack sync --all
   wits stack submit --all

``anno`` only looks up the MRs in scope, so on a stack that forks, run
``wits stack anno --all`` to give each fork-point the navigation of all its
lines.

You can also name a branch to anchor on, instead of checking it out — useful
for driving another stack from a worktree or a dirty tree:

.. code-block:: sh

   wits stack submit feature-api         # submit the stack around feature-api
   wits stack sync feature-api           # push that stack, without switching to it
   wits stack anno feature-api --all     # refresh feature-api's whole stack

The branch is a **scope anchor**, not the single target: the stack around it is
operated on exactly as if you had checked it out (an anchor mid-line still
pulls in its ancestors and downstream chain), and ``--all`` widens that to the
anchor's whole stack. That is the per-stack meaning — different from
``decorate``, whose branch names the one MR to touch. The anchor must be a real
branch (a local ref, or a name recorded in the machete file).

The base branch (``main``/``master``/…) is never pushed and never gets an MR,
but it does appear in the navigation chains so reviewers see the full lineage.

Drafts
~~~~~~

A mid-stack MR — one whose base is *another* branch, not the base branch — is
opened as a **draft** by default, because it should not be reviewed or merged
before the change it sits on. The MR at the bottom of the stack (targeting the
base branch) is opened ready. To open everything ready for review:

.. code-block:: sh

   wits stack submit --no-draft

MR title and body
~~~~~~~~~~~~~~~~~

A new MR's title and body come from one of the branch's commits — the newest
by default. Change which one:

.. code-block:: sh

   wits stack submit --title-source first   # oldest commit instead

(Existing MRs are never re-titled; this only seeds creation.)

Single branches
---------------

Not everything is a tall stack. A branch that is not recorded in the machete
file is treated as its own one-node stack sitting on the base branch — so
``sync`` and ``submit`` work on an ordinary feature branch with zero setup:

.. code-block:: sh

   git switch -c quick-fix
   # ... commit ...
   wits stack sync && wits stack submit

``anno`` skips a lone branch: a single MR has no neighbours to navigate to.

Labels, reviewers, and assignees
--------------------------------

``wits stack decorate`` adds labels, assignees, and reviewers to an MR. It is
**additive** — it only adds what you name and never removes anything — so it
never undoes a project's own label/reviewer bots, and re-running is safe.

Because these differ from one MR to the next, ``decorate`` works on **one MR at
a time** by default (the named branch, or the current one):

.. code-block:: sh

   wits stack decorate feature-api --label api --reviewer alice --assignee @me
   wits stack decorate              --label wip            # the current branch's MR
   wits stack decorate --all        --label stacked        # every MR of the current branch's whole stack

``--label``, ``--reviewer``, and ``--assignee`` are each repeatable; ``@me``
means you. ``--all`` covers the whole stack of the named branch, or of the
current one.

There is no config and no stored defaults on purpose. To make a project's
"always add these" behaviour, put the flags in a small per-repo script or your
CI step — since ``decorate`` is additive and idempotent, running it there
repeatedly is exactly equivalent to a default:

.. code-block:: sh

   # a repo's dev script
   wits stack sync && wits stack submit && wits stack anno
   wits stack decorate feature-api       --label api --reviewer alice
   wits stack decorate feature-ui        --label ui  --reviewer bob
   wits stack decorate feature-api --all --label stacked

Attribute changes are best-effort: an unknown label or a reviewer the platform
will not accept is warned about and skipped, without failing the rest — but a
run in which anything failed still exits non-zero.

Maintaining the stack structure
-------------------------------

``slice`` writes the stack; ``wits stack tree`` is for editing it afterwards.
Removing a branch never throws away what is stacked above it — its children
splice up to its parent, and the next ``submit`` retargets their base.

.. code-block:: sh

   wits stack tree prune              # drop entries whose branch no longer exists
   wits stack tree rm feature-b       # remove one branch (its children move up)
   wits stack tree rm feature-b --delete   # ...and delete the git branch too
   wits stack tree mv feature-c --onto main   # restack a branch (its substack moves with it)
   wits stack tree rename feature-c feature-d # follow a `git branch -m`

``tree mv`` updates the *shape* only; rebase the branch onto its new parent
yourself for the code to match. It also creates the entry if the branch was not
in the stack yet, so it doubles as "put this branch onto X".

``tree rename`` follows a branch that changed its name, keeping everything else
about its entry: its parent, its slot among that parent's children, its
substack, and its MR annotation. It is a file edit and never renames a git
branch — ``git branch -m`` has already done that, and the new name must be a
branch that exists. It deliberately does not protect the base branch, since a
renamed base is exactly the case where refusing would leave the forest naming a
branch that is gone.

.. _rename-needs-following:

After ``git branch -m``
~~~~~~~~~~~~~~~~~~~~~~~

A rename is the one branch event nothing can follow for you, and the reason is
worth knowing rather than rediscovering. Git applies ``git branch -m`` by
deleting the old ref, firing ``reference-transaction``, and only *then* creating
the new one — so at the moment any hook could react, the new name exists in no
ref, no reflog, no ``HEAD`` reflog entry and no ``packed-refs`` line. A hook can
tell that a branch disappeared while keeping its commit, which is very likely a
rename; it cannot learn what the branch became.

So the shipped ``reference-transaction`` hook recognises that shape, leaves the
entry alone rather than dropping it, and prints the command to finish the job:

.. code-block:: sh

   git branch -m feature-c feature-d
   wits stack tree rename feature-c feature-d

Skip it and nothing is lost — the entry simply names a branch that is gone,
which ``sync`` and ``submit`` pass over and ``tree prune`` cleans up.

Automating cleanup
~~~~~~~~~~~~~~~~~~

``tree prune`` is the one to reach for in automation: it needs no branch
names, is idempotent, and only drops branches whose ref is actually gone.
The ``reference-transaction`` hook shipped alongside this repository's git
hooks already calls ``tree rm`` when a branch is deleted, so on a machine using
those hooks the forest keeps itself honest. Everywhere else, run it at the end
of a branch-cleanup script:

.. code-block:: sh

   git branch -d old-feature && wits stack tree prune

or on a timer (cron / systemd / launchd) the same way you would schedule any
periodic chore. Since it is a no-op when nothing dangles, running it often is
harmless.

Previewing with ``--dry-run``
-----------------------------

``-n``/``--dry-run`` is global. It still reads from git and the forge to work
out what it *would* do, then prints the pushes, MR creations, base changes, and
navigation comments instead of performing them:

.. code-block:: sh

   wits stack submit -n
   wits stack sync -n -v        # -v also shows the underlying git commands

Configuration reference
-----------------------

All keys live under git config's ``wits.*`` namespace — forge identity and
tokens under the shared ``wits.forge.*``, and ``stack``'s own settings under
``wits.stack.*``.

.. list-table::
   :header-rows: 1
   :widths: 26 40 34

   * - Setting
     - Key
     - Notes
   * - Token (per host)
     - ``wits.forge.<host>.token``
     - Most specific; ``<host>`` is e.g. ``github.com``
   * - Token (per service)
     - ``wits.forge.<service>.token``
     - ``<service>`` ∈ github, gitlab, gitea, forgejo, codeberg
   * - Token (blanket)
     - ``wits.forge.token``
     - Last config fallback
   * - Token (env)
     - ``GITHUB_TOKEN``, ``GITLAB_TOKEN``, ``GITEA_TOKEN``,
       ``FORGEJO_TOKEN``, ``CODEBERG_TOKEN``
     - Used when no config key matches
   * - Service override
     - ``wits.forge.<host>.service``
     - Name a self-hosted host's type
   * - API base override
     - ``wits.forge.<host>.api-url``
     - For self-hosted / enterprise endpoints
   * - Branch prefix
     - ``wits.stack.prefix``
     - ``slice`` name suggestions (default: slug of ``user.name``, else
       ``stack/``)

There is intentionally **no** base-branch config key: the base is resolved from
the merge target's remote HEAD, then ``main``/``master``/``trunk``.

Per-run choices — drafts (``--no-draft``), title source (``--title-source``),
force (``--force``), scope (``--all``) — are flags, not config, because they
describe one invocation rather than a standing preference.

How it resolves things
----------------------

The short version: the **base branch** comes from the merge target's remote
HEAD (the ``upstream`` role's holder, else ``origin``'s), then
``main``/``master``/``trunk``; each
**MR's base** is its parent in the machete file (or the base branch at a
root); **cross-fork** MRs work on all platforms (GitHub by the fork's
repository id, Gitea via an ``owner:branch`` head, GitLab via its
cross-project API), including a fork the target's own organisation holds —
on Gitea that needs 1.26 or later, and Forgejo cannot open one. The full
rules, including fork scope and dynamic edits, are in
:doc:`/reference/stack-behavior`.

Troubleshooting
---------------

.. list-table::
   :header-rows: 1
   :widths: 44 56

   * - Symptom
     - Cause and fix
   * - ``no API token for …``
     - Set ``wits.forge.<host>.token`` or the platform's ``*_TOKEN`` env var.
   * - ``could not detect the forge for host '…'``
     - Self-hosted behind a custom domain: set ``wits.forge.<host>.service``.
   * - ``HTTP 301: moved to …``
     - The repository was renamed or transferred; point the remote at the URL
       it names. Redirects are not followed, since that would turn a write into
       a read that silently does nothing.
   * - ``… asks to wait Ns before retrying, longer than the 60s wits waits``
     - A rate limit whose quota refills later; retry after that time. Shorter
       waits are sat out and the request is sent again.
   * - ``submit`` fails to create an MR (*head not found* or similar)
     - The branch is not on the ``origin`` remote yet — run ``wits stack sync``
       first.
   * - A closed MR is not reopened
     - Intended: a closed/merged MR at the current commit is left alone. Pass
       ``--force`` to recreate.
   * - ``on the base branch '…'``
     - You are standing on ``main``. Check out a stack branch, or name one as
       the anchor.
   * - ``detached HEAD``
     - Check out a branch, or name one as the anchor.
   * - ``could not determine the base branch``
     - No remote HEAD and no ``main``/``master``/``trunk``. Create the branch
       or set the remote's HEAD (``git remote set-head origin -a``).
   * - ``rebase did not complete`` (from ``slice``)
     - The interactive rebase was aborted or hit a conflict; finish or
       ``git rebase --abort``, then retry.
   * - ``stack branch '…' is checked out in …`` (from ``slice``)
     - Git will not move a branch another worktree has checked out. Switch that
       worktree to another branch, then retry.
   * - ``keep the update-ref line of the current branch …`` (from ``slice``)
     - The branch you are on can only end the stack: leave its line last, or
       comment it out. Nothing was rewritten.
