.. _plugins:

Writing a ``wits`` plugin
=========================

``wits`` has no plugin registry, no config file to edit, and no recompilation
step to add a workflow. It uses the same mechanism git and cargo do: **when
``wits foo`` is not a built-in, it runs a ``wits-foo`` executable found on your
``$PATH``.** A plugin is just such an executable.

This keeps domain-specific, loosely-coupled workflows (say, a GPU test runner)
out of the core binary while still presenting them under the one ``wits``
command tree.

The contract
------------

* **Naming.** A plugin is an executable named ``wits-<name>`` on ``$PATH``.
  ``<name>`` is what the user types: ``wits-gpu`` is invoked as ``wits gpu``.
* **Precedence.** Built-in subcommands win. A ``wits-stack`` on ``$PATH`` is
  never reached, because ``stack`` is built in. Pick a name that is not a
  built-in (``wits help`` shows the reserved set).
* **Invocation.** ``wits gpu a b --flag`` execs ``wits-gpu a b --flag``:
  everything after the subcommand name is forwarded verbatim. The direct form
  ``wits-gpu ...`` also works — it is just the executable.
* **Process model.** ``wits`` replaces itself with the plugin (``exec``), so
  the plugin owns the terminal and stdin/stdout/stderr, and *its* exit status
  is what the caller sees. ``wits`` adds nothing to the environment.
* **Discovery.** ``wits help`` lists every ``wits-*`` on ``$PATH`` (minus the
  built-in applet symlinks) under a ``Plugins`` section, so an installed plugin
  is visible without being registered anywhere.
* **Language.** Any. A shell script, a Python script, a Rust binary — anything
  executable and named correctly.

There are no required flags, but honouring the shared conventions
(``-n/--dry-run``, ``-v/--verbose``) makes a plugin feel native.

Sharing the ``wits-core`` floor (in-tree Rust plugins)
------------------------------------------------------

A plugin written in Rust in this workspace can reuse the same floor the built-ins
use instead of reinventing it. Add a binary crate under ``crates/`` whose binary
is named ``wits-<name>``, and depend on ``wits-core``:

.. code-block:: toml

   # crates/wits-gpu/Cargo.toml
   [package]
   name = "wits-gpu"
   version.workspace = true
   edition.workspace = true

   [[bin]]
   name = "wits-gpu"
   path = "src/main.rs"

   [dependencies]
   wits-core.workspace = true
   anyhow.workspace = true
   clap.workspace = true

Add it to the workspace ``members`` in the root ``Cargo.toml``. The crate graph
is one-way; a plugin should pick the lowest layer that answers its question:

.. list-table::
   :header-rows: 1
   :widths: 22 78

   * - Crate
     - What it gives a plugin
   * - ``wits-core``
     - The floor: ``process`` (dry-run), ``git``, ``config``, ``jinja``,
       ``log``, ``pager``, ``editor``, ``time``, ``system``, ``path``; plus the
       ``forge``, ``remote``, ``trunk``, and ``worktree`` subsystems.
   * - ``wits-project``
     - The project registry: declarations, ``Workspace``/``Project``/``Repo``,
       ``resolve_paths``/``resolve_plan``, ``layout``, ``remotes``, ``skip``,
       ``check``, and ``Project::render_hook``. The template context stays
       internal.
   * - ``wits-build``
     - The cmake/meson/cargo backends and the ``ToolchainInjector`` seam, for a
       plugin that emits or plans build steps.
   * - ``wits-actions``
     - The ``build``/``update``/``devenv`` actions, for a plugin that drives a
       project's lifecycle in process rather than by parsing CLI output.
   * - ``wits-crypto``
     - The authenticated-encryption packet format and KDFs.
   * - ``wits``
     - The CLI binary. A plugin does not depend on it; it is a peer command,
       not a library.

A plugin that only renders text takes ``jinja::shared()``, or
``jinja::environment()`` if it adds filters of its own. A plugin that resolves
*wits project config* depends on ``wits-project``: ``resolve_plan``,
``resolve_paths``, and ``Project::render_hook`` are the entry points, and the
template context itself stays internal. Do not add a compatibility shim or make
a lower layer depend on a higher one; the full rules and the guards are in
:doc:`reference/project-containment`.

Extend the workspace's ``meson.build`` to build and install the plugin binary
the same way ``wits`` is, so ``meson install`` ships it alongside ``wits``.

An out-of-tree plugin needs none of this: it only has to be named
``wits-<name>`` and be on ``$PATH``.
