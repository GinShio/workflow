.. _commands-index:

The commands
============

Each chapter below is the getting-things-done guide for one command. They are
written to be read in any order — start with the command that matches the work
you have in front of you.

.. list-table::
   :header-rows: 1
   :widths: 20 80

   * - Command
     - Reach for it when…
   * - :doc:`transcrypt`
     - a repository needs to hold secrets and you do not want them sitting in
       history in the clear.
   * - :doc:`stack`
     - a chain of local branches wants to become a set of merge requests — and
       keep its bases right as you reorder the stack.
   * - :doc:`review`
     - you review merge requests (other people's included) and would rather do
       it from your editor or terminal than the web.
   * - :doc:`worktree`
     - you keep one git worktree per branch and want to make, inspect, and
       reclaim them in *any* repository.
   * - :doc:`project`
     - you build real source projects and are tired of retyping the same
       configure flags and watching the same build dirs collide.
   * - :doc:`build`
     - you want to configure and compile one of those projects.
   * - :doc:`update`
     - you want every repository a project owns refreshed from upstream in one
       safe step.
   * - :doc:`dotfiles`
     - your dotfiles are shared across machines through Dotdrop, and a flat
       per-host config is the wrong shape for keeping them.

.. note::

   The privacy of tooling matters: ``wits`` reaches the network **only** where
   a command says it does — ``stack`` (push / MR API), ``review``
   (``fetch`` / ``submit``), ``project`` / ``update`` (clone / fetch), and the
   forge detection that reads remote URLs. ``transcrypt``, ``worktree``,
   ``dotfiles``, and the read side of ``review`` / ``project`` never touch the
   network at all. One caveat: ``build`` itself is local, but it drives your
   build tool, and cargo / meson may fetch dependencies on their own.
