.. _guide-book:

Guide
=====

This is the how-to. It assumes you have cloned or pointed at the hooks once and
now want to know what is running under your git commands, how to steer it, and
how to make it yours. For the *why* of the design, see :doc:`design`; for every
config key with its default and its environment twin laid out as a table, see
:doc:`config`.

.. contents:: On this page
   :local:
   :depth: 2

Turning it on
-------------

Point git at the hooks directory, once per machine:

.. code-block:: console

   $ git config --global core.hooksPath /path/to/git/hooks

That is the entire install — there is no setup step, no manifest, nothing to
register. From the next command on, every repository you touch runs this
pipeline.

A couple of orientation points before the details. Most of this is automatic
and silent; you mainly notice it when a ``pre-commit`` or ``pre-push`` check
turns you away, which is the point. To watch a hook work, turn the logging up
for a single command (how the levels behave is covered under
:ref:`see-whats-running`):

.. code-block:: console

   $ WITS_HOOKS_LOG_LEVEL=3 git commit -m "test"

Run one on a well-set-up tree and it is a safe smoke test: the formatters
write their results back to the index, so cleanly staged content gives them
nothing to change — and with nothing staged at all, git itself refuses the
commit before any hook fires.

Two external tools are already riding along with nothing to configure: **Git
LFS** keeps large-object files synced on checkout, merge, commit and push, and
**git-branchless** records history for its smartlog/undo on nearly every hook.
The sections below cover the framework's own scripts; the integrations are
documented only where they need a decision from you.

How settings are read
---------------------

Every knob in this book is a git config key under the ``wits.hooks.``
namespace, and every one has an environment-variable twin. They answer different
needs and are read in this order:

* **An environment variable, if set** — the one-off for the command in front of
  you. It always wins. This is the "not this time" escape hatch.
* **The matching git config value** — the standing choice. Reach for it when
  you want a lasting per-repo or per-machine preference.
* **The built-in default** — used when neither is set.

So config is *where a preference lives*, and the environment beats it when you
want a single exception. To turn a hook off just for one commit rather than for
good, you set the one-shot; to turn it off from now on, you set config.

The environment name is a pure mechanical transform of the config key: upper
case the whole key and replace every ``-`` and ``.`` with ``_``. There is no
prefix juggling and no special case:

.. list-table::
   :widths: 52 48
   :header-rows: 1

   * - Config key
     - Environment twin
   * - ``wits.hooks.disable``
     - ``WITS_HOOKS_DISABLE``
   * - ``wits.hooks.pre-commit.disable``
     - ``WITS_HOOKS_PRE_COMMIT_DISABLE``
   * - ``wits.hooks.pre-commit.format-clang-disable``
     - ``WITS_HOOKS_PRE_COMMIT_FORMAT_CLANG_DISABLE``

Boolean keys accept the spellings git uses everywhere — ``true``/``false``,
``1``/``0``, ``yes``/``no``, ``on``/``off`` — and the polarity of every switch
lives in its own suffix, so the default of a switch is always "key unset" and
you only ever set one to move *off* that default:

* a **``-disable``** key guards a behaviour that runs **by default** — set it to
  turn that behaviour off;
* an **``-enable``** key guards a behaviour that is **off by default** — set it
  to turn it on.

Read a name and you know which it is: ``format-python-disable`` is on until you
switch it off; ``warn-protected-enable`` is off until you switch it on.

Throughout this guide, keys are written relative to their hook: a setting shown
as ``format-clang-style`` under :ref:`pre-commit <guide-precommit>` is the full
key ``wits.hooks.pre-commit.format-clang-style``. The :doc:`config` book writes
them out in full.

.. warning::

   Be careful with the *global* switch. ``wits.hooks.disable`` turns off every
   hook in every repository on the machine. Use the per-hook or per-script
   switch unless you mean the whole framework.

.. _guide-precommit:

pre-commit
----------

Fires after you stage changes and before the commit is recorded. It is the
busiest hook here and the only one, with ``pre-push``, that can reject a commit
— a failure stops the commit with a note on what to fix, so problems surface
now instead of in review. To bypass the whole hook for one commit,
``git commit --no-verify``.

The formatter
~~~~~~~~~~~~~

Keeps the tree consistently formatted without you having to think about it, so
what you commit is already clean. Each language is handled on its own — C/C++
through ``clang-format``, Rust through ``rustfmt``, Zig through ``zig fmt``,
Python through ``ruff`` (falling back to ``black`` plus ``isort``) — and a
generic pass guarantees a final newline on every other text file, trimming
trailing whitespace where that is safe. A language is handled only when its
formatter is on your ``PATH``.

The C/C++ pass is *diff-scoped* by default: clang-format is fed the whole
staged file, but may only change the lines the commit touches plus three
lines of context around each — the staged diff's hunks at ``-U3``. A one-line
fix in a file that predates the formatter stays a handful of lines instead of
arriving as a wall of unrelated churn, and the context window gives the
formatter room to keep the edited region locally consistent. Set
``wits.hooks.pre-commit.format-clang-whole-file-enable`` to hand the whole
file to clang-format instead. Rust, Zig and Python format whole files —
their formatters are whole-file tools by design.

It formats the **staged content**, not the working tree: it rewrites the version
in the index and, when your working copy has no unstaged edits, updates that
too. A partially staged file therefore keeps its in-progress changes intact —
the commit gets the formatted version, your edits are left alone.

The generic pass withholds the trailing-whitespace trim where it would corrupt
meaning, while still guaranteeing the harmless final newline. Markdown keeps
its trailing spaces (two of them are a hard line break), CSV/TSV keep theirs
(a trailing tab or space is a delimiter or an empty last field), and
``patch``/``diff`` files are left byte-exact — trimming or appending a newline
there corrupts the hunks. Most other markup (LaTeX, HTML, XML,
reStructuredText) collapses trailing whitespace anyway, so its files are
trimmed normally; the one caveat is verbatim-style blocks, which would lose a
literal trailing space — spare such a file by adding its extension to the
no-trim list.

Because each language is its own script, it is turned off through the ordinary
per-script switch (see :ref:`turning-pieces-off`) — set the key, no special
casing:

.. list-table::
   :widths: 34 66
   :header-rows: 1

   * - Key (under ``pre-commit``)
     - Effect
   * - ``format-clang-disable``, ``format-rust-disable``,
         ``format-zig-disable``, ``format-python-disable``
     - One per language, each on by default. Set one to stop formatting that
       language.
   * - ``format-generic-disable``
     - The generic trim/final-newline pass, on by default.
   * - ``format-generic-notrim``
     - A space-separated list of extra extensions (e.g. ``.tex .snap``) whose
       trailing whitespace to preserve, on top of the built-in Markdown/CSV/TSV
       set.
   * - ``format-clang-style``
     - A named style (``llvm``, ``google``, …) for C/C++ when the repo has no
       ``.clang-format`` of its own. When unset, ``clang-format`` discovers a
       nearby ``.clang-format`` and falls back to LLVM.

The linter
~~~~~~~~~~

Catches mistakes cheaply, before they reach a reviewer. Like the formatter,
each language is independent, running a fast, file-oriented static analyzer
over what you are committing — ``ruff`` (or ``flake8``) for Python,
``zig ast-check`` for Zig — and stops the commit on a finding. It works on the
**staged content**: a partially staged file is linted exactly as it will be
committed, not with your unstaged edits. A missing tool warns rather than
passing silently, so you are told a linter you asked for is not running.

Only genuinely *static* (no-build) linters belong here. Languages whose
analysis requires a compile have no entry: **Rust** has no non-compiling
linter (``clippy`` builds the crate, so it is left to CI rather than the commit
path), and **C/C++** is not linted, because accurate analysis needs a
compilation database and still carries false positives. Both are still
*formatted*, just not linted.

The two linters toggle the same way as the formatters —
``lint-python-disable`` and ``lint-zig-disable``, each on by default.

The sanity checks
~~~~~~~~~~~~~~~~~

A safety net for the mistakes that are easy to make and tedious to undo. Staging
a file triggers these in turn:

* an **unresolved merge-conflict marker** (``<<<<<<<`` / ``=======`` /
  ``>>>>>>>``) blocks the commit — the tell your merge left a fence up;
* a **symlink pointing nowhere** blocks it — usually a ``broken symlink:
  link -> target`` where the target was never staged;
* an **oversized file** blocks it — usually a fat-fingered ``git add`` of a
  build artifact or a dataset. The ceiling is configurable:

.. list-table::
   :widths: 34 66
   :header-rows: 1

   * - Key (under ``pre-commit``)
     - Default
   * - ``sanity-checks-max-file-size``
     - ``26214400`` bytes (25 MiB)

The marker guard
~~~~~~~~~~~~~~~~

Lets you plant a tripwire in your own code. Stage a line containing
``DO_NOT_SUBMIT``, ``NOCOMMIT``, or ``FIXME_BLOCKER`` and the commit is refused
until you remove it — the reliable way to make sure a debug hack or a
note-to-self never ships. The scan is over the staged content only and skips
binary files. Nothing to configure.

The secret scan
~~~~~~~~~~~~~~~

A last line against committing credentials. It scans the staged diff with
``gitleaks`` (preferred) or ``git-secrets`` and blocks on a match. There is no
switch to flip: it is active whenever one of those scanners is on your
``PATH``, so *installing the tool is how you turn it on*. A genuine false
positive can be waved through with ``git commit --no-verify``.

The encoding check
~~~~~~~~~~~~~~~~~~

Keeps staged text honest: LF newlines only, and valid UTF-8. A staged text file
carrying a CR or CRLF line ending, or an invalid UTF-8 byte, is rejected.
Binary blobs are skipped, and so is the UTF-8 half when ``iconv`` is not
installed. Nothing to configure — for automatic newline normalisation on top of
this, let git do it with a ``.gitattributes`` entry of ``text=auto eol=lf``.

The protected-branch prompt
~~~~~~~~~~~~~~~~~~~~~~~~~~~

A guard against committing straight onto a shared branch by accident. Enabled,
it asks you to confirm before committing while a protected branch is checked
out. Off by default, since whether a direct commit is a mistake depends
entirely on how you work:

.. code-block:: console

   $ git config wits.hooks.pre-commit.warn-protected-enable true

It reads the current branch, and only prompts when that branch matches the
protected pattern. A loud ``[WARN]`` announces the situation, then a prompt
appears; answering anything but ``y``/``yes`` aborts the commit. The answer is
read from the terminal itself, so where there is no one to ask — a script, a
CI run — the prompt declines and the commit is aborted.

Which branches count as protected is shared with the ``pre-push`` prompt and is
set framework-wide once:

.. list-table::
   :widths: 30 70
   :header-rows: 1

   * - Key
     - Meaning
   * - ``wits.hooks.protected-branch``
     - An extended regular expression (matched with ``grep -E``) a branch name
       must match to be treated as protected. Default
       ``^(main|master|dev|release-.*|patch-.*)$``. Environment twin
       ``WITS_HOOKS_PROTECTED_BRANCH``.

prepare-commit-msg
------------------

Runs as git assembles the initial commit message, before your editor opens —
the moment to seed it with something.

The issue-ID prefix
~~~~~~~~~~~~~~~~~~~

Saves retyping a ticket number into every commit. Enabled, it reads an issue ID
out of the current branch name and puts it in the message, so on
``feature/PROJ-123-login`` your subject opens with a ``[PROJ-123]`` prefix
already in place:

.. code-block:: console

   $ git config wits.hooks.prepare-commit-msg.issue-tracker-enable true
   $ git checkout -b feature/PROJ-123-login
   $ git commit            # editor opens with "[PROJ-123] " in front of the subject

It applies to nearly every message-producing command — a plain commit, ``-m``,
a ``-t`` template, a merge, a cherry-pick, a revert, an amend — but stays out
of the way where acting would be wrong:

* during a **rebase**, replayed or reworded commits already carry their ID, so
  rewriting history messages would be surprising — it stays out;
* on an **autosquash marker** (``fixup!``/``squash!``/``amend!``), prefixing it
  would stop ``git rebase --autosquash`` from matching it — it stays out;
* on a **detached HEAD** there is no branch, so no ID to read — it stays out;
* and it never adds itself **twice** — a message that already carries the ID is
  left as is.

By default the ID is **prepended** to the subject as ``[PROJ-123] ``. It can
instead be **appended** as a git trailer at the end of the message
(``Refs: PROJ-123``) — the right shape for tools that read trailers.

The five knobs, all under ``prepare-commit-msg``:

.. list-table::
   :widths: 30 70
   :header-rows: 1

   * - Key
     - Meaning and default
   * - ``issue-tracker-enable``
     - Off by default; set true to turn it on.
   * - ``issue-tracker-regex``
     - What to pull from the branch name; default ``[A-Z]+-[0-9]+``. The first
       match wins.
   * - ``issue-tracker-position``
     - ``prepend`` (default) puts the ID at the start of the subject;
       ``append`` adds it as a git trailer at the end of the body, placed
       correctly relative to git's comment block.
   * - ``issue-tracker-format``
     - How to wrap the ID; the first ``%s`` is replaced by the ID and
       everything else is taken literally. The default follows the position:
       ``[%s] `` for ``prepend``, ``Refs: %s`` for ``append`` (which should
       stay trailer-shaped, ``Token: %s``).
   * - ``issue-tracker-default``
     - A fallback ID to use when the branch name carries none. In ``append``
       mode this placeholder is added only when no issue trailer exists yet —
       it never stacks onto a real, branch-derived reference — whereas a
       branch-derived ID accumulates a distinct value.

The provenance trailer
~~~~~~~~~~~~~~~~~~~~~~

Records where a *derived* commit came from as a structured git trailer, so the
link is machine-readable instead of buried in prose (or, for a cherry-pick
without ``-x``, absent entirely):

.. list-table::
   :widths: 22 78
   :header-rows: 1

   * - Operation
     - Trailer written
   * - merge
     - ``Merges: <sha>`` — one per parent, so an octopus merge lists them all.
   * - cherry-pick
     - ``Cherry-picked-from: <sha>``.
   * - revert
     - ``Reverts: <sha>``, *replacing* git's default
       ``This reverts commit <sha>.`` line.

The operation is detected from git's in-progress marker files
(``MERGE_HEAD``, ``CHERRY_PICK_HEAD``, ``REVERT_HEAD``), so it works for a
cherry-pick or revert done without an editor. It stays out of a ``rebase``, and
the trailer is idempotent on a re-edit. On by default — it only touches
merge/cherry-pick/revert and is otherwise additive, so there is nothing to
switch and nothing that would surprise you.

pre-push
--------

Runs before git hands refs to a remote, and can abort the push.

The protected-branch prompt
~~~~~~~~~~~~~~~~~~~~~~~~~~~

The push-side counterpart to the commit prompt: enabled, it asks for
confirmation before pushing to a protected branch on the remote. Off by
default. It reads each ref about to be pushed from stdin, ignores branch
deletions and non-branch refs (tags, notes), and prompts once per push if any
remote branch matches the protected pattern:

.. code-block:: console

   $ git config wits.hooks.pre-push.warn-protected-enable true

The protected set is the same shared
:ref:`guide-precommit` regex as the commit prompt. To bypass it for one push,
``git push --no-verify``. And as with the commit prompt, the answer is read
from the terminal: a push with no terminal to ask — CI, a scripted push — is
declined.

It hands off to Git LFS next, uploading the large objects the push depends on,
so a push never completes with half-uploaded LFS content.

post-checkout
-------------

Runs after ``git checkout``/``switch`` and after ``clone``. Everything here is
advisory — it never blocks the operation, it just keeps your working
environment in step with the branch you moved to.

The workspace restore
~~~~~~~~~~~~~~~~~~~~~

Keeps your editor pointed at the right build as you jump between branches. For
an out-of-tree build it repoints the working tree's ``compile_commands.json``
at the active branch's build directory, so language servers and clang-tooling
index the branch you are actually on. It only acts on branch checkouts (not
file checkouts), only manages that path when it is a symlink or absent, and
never clobbers a real file you keep in-tree. When no build directory exists yet
for the branch, it warns and leaves things alone. Nothing to configure — the
build directory itself is resolved through the ``wits`` project registry.

The encrypted-file modes
~~~~~~~~~~~~~~~~~~~~~~~~

Locks the files that encrypting clean/smudge filters own — transcrypt,
git-crypt, and friends — to ``0600`` in the working tree. Git tracks only the
executable bit of a file's mode, so a checkout materialises the rest from your
umask: a transcrypt secret lands ``0644`` no matter what its author intended,
and the filter itself cannot fix it, because clean/smudge see a byte stream,
never the file. This script re-asserts the mode at the moment the tree changes.
Because ``600`` vs ``644`` differ only in bits the index drops, ``git status``
stays quiet — the chmod never dirties your tree. The same script also runs on
``post-merge`` and ``post-rewrite`` (those entries are symlinks to it), since a
merge or rebase can surface freshly smudged secrets.

The dependency-change warning
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

A nudge so you do not run against stale dependencies. When a checkout changes a
lockfile — ``package-lock.json``, ``Cargo.lock``, ``go.sum``, ``poetry.lock``,
and the rest — it reminds you to reinstall. It only ever warns; it will not run
your package manager for you. The watcher matches by basename anywhere in the
The watcher matches by basename anywhere in the
tree, so the exact path does not matter. (The same script also runs on
``post-merge`` and ``post-rewrite``, comparing the tree before the operation
— git's ``ORIG_HEAD`` after a merge, the rewritten commits' old tips on the
hook's stdin after a rebase or amend — to the current one.)

The maintenance enrolment
~~~~~~~~~~~~~~~~~~~~~~~~~

Opts each repository into git's own upkeep. The first time you land in a repo
it registers it with ``git maintenance start``, so git's background tasks
(commit-graph, gc, and so on) keep the repository fast without you scheduling
anything. It is a no-op once the repository is already registered. Nothing to
configure.

The branchless bootstrap
~~~~~~~~~~~~~~~~~~~~~~~~

Sets up ``git-branchless`` for a repository the first time you check out a
branch there — typically right after cloning — when branchless is installed and
not already initialised, so a fresh clone is ready without a manual
``git branchless init``. Its generated stubs go to a dedicated inactive
directory; the shared ``core.hooksPath`` entrypoints remain the only active
dispatch path. Everything here is idempotent: once ``branchless`` is set up, it
does nothing.

reference-transaction
---------------------

Fires whenever refs change. The scripts here react to one case in particular —
branch deletion — and act only once the change is actually committed, so an
aborted rebase or a rolled-back transaction never triggers them. A branch
rename — which git applies as a deletion of the old name plus a creation of
the new one, pointing at the same commit — is recognized by that pairing and
left alone.

The machete cleanup
~~~~~~~~~~~~~~~~~~~

Keeps your ``git-machete``/stack layout honest as branches come and go. Delete
a branch and it is removed from the machete definition file with its children
spliced up to its parent, so the tree stays valid instead of collecting
dangling entries you would have to prune by hand. Where ``wits`` is installed,
it owns the edit (``wits stack tree rm``); when the tool is missing or the
edit fails, a built-in rewrite does the same splice. The file lives in the
common git dir, so the cleanup reaches
the same forest from any worktree of the repository. Runs wherever a machete
file exists; nothing to configure.

The build-directory cleanup
~~~~~~~~~~~~~~~~~~~~~~~~~~~

Reclaims disk when you delete a branch. Opt in, and deleting a branch also
removes its build directory (resolved through the ``wits`` project registry).
Off by default because it deletes files — enable it per repository where you
want the housekeeping:

.. code-block:: console

   $ git config wits.hooks.reference-transaction.cleanup-build-dir-enable true

It refuses to remove anything it cannot prove is a build directory — the
resolution checks a candidate is a real directory, not a symlink, not your
home or the repository root, and not shared with the main branch — so a stray
branch name can never point ``rm -rf`` at something you care about.

The recorder hooks
------------------

``post-commit``, ``post-merge``, ``post-applypatch``, ``post-rewrite``, and
``pre-auto-gc`` mostly exist to feed the pass-through integrations —
``git-branchless`` on all of them, Git LFS on ``post-commit`` and
``post-merge``. ``post-merge`` additionally reuses the encrypted-file-modes
and dependency-change scripts from ``post-checkout``; ``post-rewrite`` likewise
runs both, since a rebase or amend can surface freshly smudged secrets — and
the lockfiles the upstream you just rebased onto has moved. There is nothing
framework-specific to configure on
any of them — they do their one job and get out of the way.
any of them — they do their one job and get out of the way.

.. _turning-pieces-off:

Turning pieces off
------------------

Beyond the per-behaviour switches throughout this guide, you can silence things
wholesale — when a hook is wrong for a repository, or simply in your way this
once. Three scopes, from widest to narrowest:

.. list-table::
   :widths: 34 66
   :header-rows: 1

   * - Scope
     - Key
   * - Everything
     - ``wits.hooks.disable``
   * - One hook
     - ``wits.hooks.<hook>.disable``, e.g. ``wits.hooks.pre-commit.disable``
   * - One script
     - ``wits.hooks.<hook>.<script>-disable``, naming the script without its
       numeric prefix, e.g. ``wits.hooks.pre-commit.format-python-disable``

As everywhere, config is the standing choice and the environment variable is
the one-shot that wins for a single command:

.. code-block:: console

   $ git config wits.hooks.pre-commit.disable true      # this repo, from now on
   $ WITS_HOOKS_PRE_COMMIT_DISABLE=1 git commit ...     # only this commit

The scopes only narrow what they silence: there is no precedence to weigh, and
any matching switch that is set turns the behaviour off — the way back on is to
unset it. Turning off a whole hook turns off every script under it; turning off
one script leaves its siblings running.

Extending the pipeline
----------------------

When a hook fires, its scripts run in a fixed order — each stage additive, and
fail-fast (the first non-zero exit stops the hook):

1. the built-in ``<hook>.d/`` scripts, in filename order;
2. any **overlay layers** (``secret-*``, below);
3. the **external hooks** you point the framework at;
4. the repository's own ``.git/hooks/<hook>``, if it still has one.

A candidate runs only if it is **executable** and its first line is a ``#!``
shebang — anything else (a data file, or a still-encrypted blob) is skipped
rather than executed. Every stage obeys the same disable hierarchy above.

Adding a built-in check
~~~~~~~~~~~~~~~~~~~~~~~

Every behaviour is one executable, so adding one is dropping a file into the
right ``<hook>.d/``. The formatter and linter are split one-language-per-file
precisely for this: to add a language, copy the closest sibling, point it at
the tool, and list the extensions. The ``staged_lang_files`` helper does the
staged/text/binary/encrypted filtering, so the script stays a few lines:

.. code-block:: sh

   #!/bin/sh
   . "$HOOKS_DIR/core/lib.sh"
   files=$(staged_lang_files .go)          # staged .go files, text only
   [ -n "$files" ] || exit 0
   command -v golangci-lint >/dev/null 2>&1 || exit 0
   # ... run the tool on the staged content, exit non-zero to block the commit

Peers of one concern share a numeric prefix (every formatter is ``50-``, every
linter ``60-``); the prefix only sets run order, so the shared number just
means "these are the same stage." The clean name (prefix stripped) becomes the
per-script toggle key automatically — a ``60-lint-go`` answers to
``wits.hooks.pre-commit.lint-go-disable`` with no extra wiring. Nothing to
register.

External hooks
~~~~~~~~~~~~~~

If a repository already carries hooks, they can run in the same pass instead of
being displaced. Two ways to point at them; both accept colon-separated
entries, absolute or relative to the repo root, and both run *after* the
built-in and overlay scripts, so a project's own checks get the last word.

**Directories to scan** — ``wits.hooks.external-dirs``. In each directory, for
a given hook, the framework runs whichever of these it finds:

* a single executable file named exactly ``<hook>`` — the Husky convention
  (``.husky/pre-commit``);
* a ``<hook>.d/`` directory, whose executable scripts run in filename order,
  exactly like the built-in ones (``.githooks/pre-commit.d/``).

**Explicit scripts** — ``wits.hooks.<hook>.external-scripts``, for a project
whose scripts do not follow the ``dir/<hook>`` convention. They run in the
order listed.

.. code-block:: console

   $ git config wits.hooks.external-dirs ".husky:.githooks"
   $ git config wits.hooks.pre-commit.external-scripts "scripts/lint.sh:tools/check-fmt"

A scanned directory's single-file hook is toggled under the pseudo-name
``external`` (``WITS_HOOKS_PRE_COMMIT_EXTERNAL_DISABLE=1``); explicit scripts
and a directory's ``.d/`` scripts are toggled under their own filename. The
repository's own ``.git/hooks/<hook>`` — the last layer — answers to the
pseudo-name ``local`` the same way (``wits.hooks.<hook>.local-disable``).

Overlay layers (``secret-*``)
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

Alongside the hooks directory you can drop overlay directories named
``secret-*``, each mirroring the main layout (``secret-<name>/<hook>.d/``).
Their scripts run right after the built-in ones and before any external hooks —
a home for private or machine-specific hooks you do not want in the shared
tree.

This is also where a **transparently-encrypted** hook belongs. Keep a script
under ``secret-<name>/<hook>.d/`` and encrypt it with transcrypt: it sits in
the repository as ciphertext, and because a script only runs when it starts
with a ``#!`` shebang, the encrypted (shebang-less) blob is quietly skipped
until it is decrypted on checkout — after which it runs like any other. So a
private hook can travel in the repository without exposing its contents, and
without erroring on a machine that cannot decrypt it.

.. _see-whats-running:

Seeing what a hook is doing
---------------------------

When something surprises you, turn up the logging for the next command. The
level is set by the ``WITS_HOOKS_LOG_LEVEL`` environment variable and runs from
silent to a full shell trace:

.. list-table::
   :widths: 10 90
   :header-rows: 1

   * - Level
     - What you see
   * - ``0``
     - Nothing at all.
   * - ``1``
     - Errors only — the failures that abort a hook.
   * - ``2``
     - Errors and warnings (the default).
   * - ``3``
     - ...plus the informational notes — what got formatted or linked, what was
       registered.
   * - ``4``
     - ...plus a debug trace of each candidate considered and skipped, and a
       shell trace of the scripts.

.. code-block:: console

   $ WITS_HOOKS_LOG_LEVEL=3 git commit ...   # 0 silent · 1 errors · 2 warnings (default) · 3 info · 4 trace

All diagnostics go to **stderr**, so they stay visible even when a hook's
stdout is captured or piped.

Troubleshooting
---------------

Common situations and where to look:

* **A commit you never expected gets rejected.** Read the last ``[ERROR]`` line
  and follow its instruction — it names the file and the problem. A protected
  branch or a marker is the usual cause; ``git commit --no-verify`` bypasses
  the whole ``pre-commit`` for a known-false-positive case.
* **Nothing seems to run at all.** Check ``git config core.hooksPath`` points
  at this directory and that the hook files are executable. Bump
  ``WITS_HOOKS_LOG_LEVEL=3`` to confirm the runner is firing.
* **A formatter or linter is missing without complaint.** Most tools skip
  silently when absent by design; linters warn. Install the tool, or accept the
  gap — the pipeline shrinks to what exists.
* **LFS or branchless events seem to vanish.** Both integrations skip
  themselves quietly when their tool is missing. A branchless recorder that
  fails warns and carries on; a failed LFS sync stops the hook — that is the
  guarantee behind "a push never completes with half-uploaded LFS content".
* **The machete file or a build directory changed behind your back.** That is
  ``reference-transaction`` reacting to a committed branch deletion — by design.
  Disable the specific script if you do not want it.
