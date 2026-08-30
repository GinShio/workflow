wits
=====

.. _wits-index:

A personal toolbox that lives under one command.

``wits`` is a single binary that collects workflow tools you would otherwise keep
as a pile of loose scripts behind one command tree. The point of the collection
is exactly what a collection buys you: a shared library, consistent flags, one
thing to build, and one command on your ``$PATH`` that does everything the loose
scripts were doing separately.

This repository only ever contains what is actually finished. Nothing
half-built is published, and a new subcommand arrives the same way everything
else did: as a working, documented, tested piece of the whole.

.. note::

   The noun for a *pull request* varies by platform (GitHub calls it a PR,
   GitLab a merge request). ``wits`` talks to both, so it says **MR**
   everywhere in its own output and documentation, and shows the host's word
   where the host's UI is involved.

What is inside
--------------

The built-in command tree today:

==============  =============================================================
Command         What it does
==============  =============================================================
``transcrypt``  Transparent file encryption wired into git's clean/smudge
                filters: commit ciphertext, check out plaintext, forget about
                it.
``stack``       Turn a chain of branches into a set of merge requests, and
                keep them in sync as you reshape the stack.
``review``      Review merge requests locally, across forges: fetch, comment,
                give a verdict, submit everything as one batch.
``worktree``    Create, inspect, and reclaim git worktrees in any repository —
                submodules borrowed, never re-downloaded.
``project``     Describe and validate source projects from one declarative
                registry, and answer machine-readable path queries for scripts
                and git hooks.
``build``       Configure and build a project on top of that registry
                (cmake / meson / cargo).
``update``      Refresh git for every repository a project owns.
``dotfiles``    Compile a TOML manifest tree into per-host Dotdrop
                configurations.
==============  =============================================================

One *plugin* ships in the same workspace:

==============  =============================================================
Plugin          What it does
==============  =============================================================
``scaffold``    Copy facts from the Vulkan and SPIR-V specifications into the
                repeated boilerplate a source tree expects.
==============  =============================================================

Anything else is a plugin of your own: ``wits anything`` runs a ``wits-anything``
executable from ``$PATH``, git-style, so a domain-specific workflow plugs in
without ever being compiled into ``wits``.

How to read this book
---------------------

The documentation is arranged the way the tool is: the chapters under
:doc:`commands/index` are the getting-things-done guides, one per command.
Each guide covers the mental model, the setup, the verbs and their flags, the
configuration, and the failure modes you will actually meet.

Under :doc:`reference/index` sit the deeper references — the exhaustive
configuration and flag tables for the project system, the JSON contract editors
talk to, and the design notes that record *why* each tool is shaped the way it
is.

Start with one of these paths, whichever matches what you came for:

* **Just installed it** — :doc:`installation`.
* **A repository has secrets it should not commit** — :doc:`commands/transcrypt`.
* **Branches keep growing into a stack nobody can review** — :doc:`commands/stack`.
* **You review other people's MRs and want to do it locally** — :doc:`commands/review`.
* **One branch per build, without re-checking-out the world** — :doc:`commands/worktree`.
* **A monorepo whose build flags you keep retyping** — :doc:`commands/project`.
* **Dotfiles that need different content per machine** — :doc:`commands/dotfiles`.
* **A new Vulkan extension that has to appear in seven places** — :doc:`scaffold`.
* **The shared floor** — the mechanism chapter, :doc:`concepts`, explains the
  floor every command stands on: process, git, config, crypto, log, template,
  time, and host facts.

Conventions
-----------

Flags are spelled ``--long`` with ``-short`` aliases where they exist. Config
keys are shown with their exact dot-separated spelling. Environment variables
are written in the ``NAME`` form and also appear in the references as
``VARIABLE``.

.. toctree::
   :maxdepth: 2
   :caption: The book

   installation
   concepts
   commands/index
   commands/transcrypt
   commands/stack
   commands/review
   commands/worktree
   commands/project
   commands/build
   commands/update
   commands/dotfiles
   plugins
   scaffold
   reference/index
   reference/project-reference
   reference/project-design
   reference/stack-behavior
   reference/stack-design
   reference/review-json
   reference/review-store
   reference/review-design
