.. _project-containment:

Project containment
===================

This is the architecture charter for the project subsystem: which crate owns
which question, what each may depend on, and where the boundaries are enforced.
It is deliberately short. The reasoning behind the domain model lives in
:doc:`project-design`; every configuration key, flag, and query is in
:doc:`project-reference`.

The crate graph
---------------

.. code-block:: text

   wits-core
     process config path jinja log pager editor time system
     git remote forge worktree trunk
         ▲
     wits-project
         ▲
     wits-build
         ▲
     wits-actions
         ▲
     wits (bin)                 (core, project, build, actions, crypto)

   wits-crypto                  (leaf: no wits-* crate)
   wits-scaffold (bin)          (core)

.. list-table::
   :header-rows: 1
   :widths: 16 30 28 26

   * - Crate
     - Owns
     - May depend on
     - Must not depend on
   * - ``wits-core``
     - the OS/git/process/path floor, ``git``, ``remote``, ``forge``,
       ``worktree``, ``trunk``
     - external crates only
     - any ``wits-*`` crate
   * - ``wits-project``
     - the declaration model, the registry, resolution, path and plan
       queries
     - ``wits-core`` and external crates
     - ``wits-build``, ``wits-actions``, ``wits-crypto``
   * - ``wits-build``
     - cmake/meson/cargo backends and their ``ToolchainInjector`` impls
     - ``wits-project`` and external crates
     - ``wits-actions``; ``wits-core`` outside dev-dependencies
   * - ``wits-actions``
     - the build, update, and devenv actions
     - ``wits-core``, ``wits-project``, ``wits-build``
     - nothing above it
   * - ``wits-crypto``
     - the packet format, KDFs, and filter entry points
     - external crates only
     - any ``wits-*`` crate
   * - ``wits``
     - the CLI: parsing, output, dispatch, and the command shells
     - all of the above
     - —
   * - ``wits-scaffold``
     - the Vulkan/SPIR-V scaffolding plugin
     - ``wits-core`` and external crates
     - ``wits-project``, ``wits-build``, ``wits-actions``, ``wits-crypto``

The rule behind the graph is one sentence: **a crate may depend only on the
layer below the question it answers.** The dependency direction is what keeps a
read-only query from pulling in a build system, and what keeps the CLI from
becoming the only place a behavior is reachable.

The project layer's two halves
------------------------------

``wits-project`` is one crate on purpose. The monolith guard is the module
split, not Cargo:

* ``decl`` and ``load`` parse and validate the file model. They do not render a
  path and do not query git.
* ``workspace``, ``layout``, ``paths``, and ``plan`` resolve.
  ``Workspace::load`` may read git while resolving a borrow's checkout and
  validating layout.
* ``context``, ``presets``, and ``toolchain`` are resolution policy. ``Ctx`` is
  internal: consumers get ``BuildPlan::get_scalar`` and
  ``Project::render_hook``, never a context handle. The path stage answers in
  typed ``ResolvedPaths`` fields, so a template query has one answer — the full
  plan's.

The two stages are types, not a flag:

* ``ResolvedPaths`` answers a checkout question: every repo's ``workdir``,
  ``source_dir``, ``build_dir``, ``install_dir``, and the selected toolchain.
* ``BuildPlan`` adds the configuration layers: toolchain injection, org,
  project, presets, CLI extra args, and the accumulated ``LogicalConfig``.

There is no ``paths_only`` switch. A caller names the question it has by naming
the type it asks for. ``info --get`` deliberately stays on ``BuildPlan`` because
its contract includes the layers; ``branch-build-dirs`` deliberately stays on
``ResolvedPaths`` because it only needs a path.

What a consumer asks
~~~~~~~~~~~~~~~~~~~~

.. list-table::
   :header-rows: 1
   :widths: 36 64

   * - Question
     - Entry point
   * - Which project owns this path?
     - ``Workspace::project_for_path`` / ``repo_for_path``
   * - What does a command's positional name, and which repo does it act on?
     - ``resolve_target`` → ``Target::build_focus`` / ``Target::checkout_focus``
   * - Where is a repo's repository, and where is a checkout?
     - ``Project::repo_abs_path``, ``layout::primary_path``,
       ``layout::work_dir``, ``layout::checkout_holding``,
       ``layout::primary_checkout``
   * - What is the full build plan?
     - ``resolve_plan``
   * - What are the paths only?
     - ``resolve_paths``
   * - Which remotes hold which role?
     - ``remotes::for_checkout``, ``declared_for_checkout``,
       ``roles_in_force``
   * - Is a ``skip`` list in force?
     - ``skip::violations`` / ``skip::remedy``
   * - Is the configuration legal?
     - ``check_one`` / ``check_all``
   * - What is a lifecycle hook's command?
     - ``Project::render_hook``

The action boundary
-------------------

``wits-actions`` owns the mutating verbs; the CLI only parses arguments and
formats output. An ``Identity`` is resolved before an action runs, so an
in-process orchestrator can reuse ``build``/``update``/``devenv`` without a
command line. Neither the actions nor the project layer read the process cwd:
where the caller stands arrives as ``Identity::here`` / ``PathInput::here``,
and only the CLI's target funnel (``resolve_target``) turns "no positional"
into the current directory.

``wits-build`` is the only extension axis. ``wits-project`` names no backend,
``Step``, ``EmitContext``, or build-system registry: it knows the one-method
``ToolchainInjector`` seam, and each backend implements it. Adding a build
system is a backend impl and an enum variant, not a change in the project layer.

What crosses, and what does not
-------------------------------

``wits-project`` may use ``wits_core::git``, ``wits_core::remote``,
``wits_core::trunk``, and the floor. It must **not** use ``wits_core::forge`` or
``wits_core::worktree``: those are command concerns — the CLI and
``wits-actions`` need them, the project layer does not. Cargo cannot enforce
that split because all three live in ``wits-core``, so
``crates/wits-project/tests/boundaries.rs`` scans the code of the project
sources for anything that reaches either module: a path through it, an import
group naming it, or a glob or alias of ``wits_core`` that would hide it.
Comments are not code and are skipped. That test is part of ``cargo test
--workspace``.

``wits-build`` does not name ``wits-core`` in its normal dependencies. It
receives project types and implements the core-owned seam. ``wits-actions`` may
use all three lower layers. ``wits`` may use all of them. ``wits-scaffold``
depends, among ``wits`` crates, on ``wits-core`` only.

``wits-crypto`` is a leaf: it names no other ``wits`` crate and is used only by
``transcrypt`` and the dotfiles commands.

The accepted cost
~~~~~~~~~~~~~~~~~

``forge`` lives in ``wits-core``, so a project-only consumer transitively links
the forge's HTTP stack (``ureq``, and through it ``ring``). That is the price of
keeping the crate graph small, and it is accepted: the boundary test prevents
the project layer from *calling* forge, and a feature gate is the fallback if
the linkage becomes a real build-weight problem. It is not added pre-emptively.

``wits-project`` remains one crate for the same reason: splitting declaration
from resolution into separate crates would add a Cargo boundary where a module
boundary already answers the question, and the two halves are always consumed
together.

What deliberately does not exist
--------------------------------

* No compatibility crate or re-export facade for the removed utility layer.
* No ``wits-registry`` crate: declaration and resolution live in
  ``wits-project``, separated by modules and visibility.
* No public ``Ctx``: the template engine is policy, not API.
* No build-system types in ``wits-project``: only the injector seam.
* No cross-project build graph. A ``from`` borrow carries a repo's identity, not
  another project's build; an orchestrator belongs above ``wits-actions``.
* No ``paths_only`` flag: the two stage types are the contract.

How the boundary is checked
---------------------------

.. code-block:: sh

   cargo test --workspace        # includes the wits-project boundary scan
   # core + anyhow/log/minijinja/serde/toml/thiserror
   cargo tree -p wits-project
   # the narrow crates, without the removed utility layer
   cargo tree -p wits
   meson compile -C _build       # the workspace still builds and installs
   wits project check            # the deployed registry still validates

No reference to the removed utility layer remains in the tree; a new one is a
regression, not a compatibility shim.

Where to read next
------------------

* :doc:`project-design` — the domain model and the reasoning behind it.
* :doc:`project-reference` — every key, flag, query, and output contract.
* :doc:`/plugins` — which crate an in-tree plugin should depend on.
