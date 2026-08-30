.. _wits-update:

``wits update``
===============

Refresh git for every repository a project owns — the one safe sweep that
keeps a whole project's checkouts current. The registry it reads is
:doc:`project`'s; this chapter is only about the refresh itself.

.. code-block:: sh

   update hello                    # one project's repos
   update                          # every project
   update hello --with-borrowed    # include repos owned by another project

What it does with each repo
---------------------------

For each repo in the project, in dependency order (parents before nested):

* **Missing path → clone.** The repo is cloned the first time. In-place
  defaults to ``git clone`` from the sync source; worktree/hybrid build a
  *tracking bare host* — ``git init --bare``, ``git remote add``,
  ``git fetch --tags``, then ``main_branch`` created from the remote's branch
  as the repository's symbolic HEAD — and add its bootstrap worktree. Deliberately
  **not** ``git clone --bare``, which copies every remote branch into
  ``refs/heads``, writes no fetch refspec, and publishes no ``origin/HEAD``
  (see :doc:`worktree`). Submodules are initialised, ``skip`` applied, and the
  result verified.
* **Existing → update.** Remotes are ensured (additive only — including a
  fetch refspec for a remote that has none, which is how a repository cloned
  with ``git clone --bare`` is repaired), then the default action runs.

The **sync source** is ``upstream`` if declared, else ``origin``. Cloning
names the fetched remote after the sync source, so tracking an ``upstream``
leaves ``origin`` free for a fork.

The default action

* **On ``main_branch``:** ``git fetch <sync>`` then
  ``git merge --ff-only <sync>/<main_branch>``.
* **Otherwise:** a ref-only fast-forward — ``git fetch <sync>
  <main_branch>:<main_branch>`` — which does not check out, does not touch the
  working tree, and does not expand a sparse checkout.
* **Bare-backed:** ``git fetch <sync>``, then fast-forward whichever linked
  worktree holds ``main_branch``; if none remains, advance the local branch
  ref with ``update-ref``, refusing anything that is not a fast-forward.
  Nested repo lifecycle work is skipped until a main worktree exists again.
* Declared submodule repos advance via their own lifecycle; undeclared nested
  submodules are refreshed with ``git submodule update --recursive -- <materialised
  paths>`` — no ``--init``; ``--init`` happens only on clone or worktree
  creation.

Safe by default
---------------

Three properties make ``update`` safe to run unattended:

* **Nothing is switched.** A feature checkout advances the main ref without
  checking it out; a bare-backed repo updates the worktree already holding
  main, or advances only the bare ref. No path is ever re-pointed just to
  update.
* **A sparse checkout is never expanded.** The refspec fetch, the ``--ff-only``
  merge, and the limited submodule refresh all honour the cone.
* **Remotes are additive.** Missing remotes and mirror push-URLs are added;
  existing URLs are never modified or removed; remotes the config does not
  mention are untouched. A URL you set by hand is yours, and ``update`` never
  "corrects" it.

Borrowed repos stay with their owner
------------------------------------

``update`` skips repos declared with ``from``, so a component five projects
consume is fetched once by its owner rather than five times. ``update <project>
--with-borrowed`` opts in when you specifically want the sweep.

``skip`` verification
---------------------

Before refreshing a repo, ``update`` verifies that repo's declared ``skip``
mask is actually in force — a contradicted ``skip`` is a hard error, not a
refresh:

.. code-block:: sh

   update viewer
   # [ERROR] repo 'main': skipped path 'third_party/engine' is materialised …
   # [INFO] re-run with -v to see the commands that fix this

   update viewer -v          # prints the exact deinit + sparse-checkout commands

``update`` never *applies* the mask itself: doing that to a tree wits did not
build means deleting content, which is yours to do (the ``-v`` commands show
you exactly what that would be). ``clone`` is the one place the mask is
applied, because the tree is still being built.

Hooks
-----

Each repo may declare inline ``sh -c`` hook strings for any phase. The phases:

* ``clone`` / ``post_clone`` — around the creation of the repo (there is no
  ``pre`` hook: before the repo exists there is nothing useful to run in). A
  ``clone`` override runs in the **current working directory** and owns both
  repository and bootstrap creation.
* ``pre_update`` / ``update`` / ``post_update`` — around the refresh. A bare
  ``update`` override replaces the default action wholesale; ``pre_``/``post_``
  add hooks around it. Update hooks run in the checkout being refreshed, or in
  a bare repo's main worktree when one exists, falling back to the bare path.

The same fail-fast discipline applies everywhere: a hook or action exiting
non-zero stops the whole operation, the RAII guard returns the repo to its
original branch (and pops any stash it made), a log line records the failure,
remaining repos are skipped, and the process exits non-zero. State is never
left half-switched.
