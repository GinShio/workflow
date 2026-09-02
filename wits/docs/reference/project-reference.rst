.. _project-reference:

``wits project`` — reference
=============================

The exhaustive reference: every configuration key, every CLI flag, the
template language, and the resolution rules. For a gentle introduction read
:doc:`/commands/project`; for rationale read :doc:`project-design`.

.. note::

   This documents the contract the code upholds. Where a detail has evolved in
   code, the code is authoritative.

.. contents:: On this page
   :local:

CLI
---

::

   project [<name|path>] [--check] [--focus <repo>] [profile flags]
   build   [<name|path>] [--focus <repo>] [profile flags] [--detach] [build options]
   update  [<name|path>] [--with-borrowed]

``--with-borrowed`` also refreshes repos declared with ``from``, which
``update`` otherwise leaves to the project that owns them.

Worktrees are not ``project``'s: :doc:`/commands/worktree` creates and
reclaims them for any repository. The two meet only at a path — ``project
work-dir`` resolves/discovers one, ``build --work-dir`` accepts any.

Machine-readable queries for scripts and git hooks::

   project exists       <name>
   project main-branch [<name|path>]
   project build-dir   [<name|path>] [--branch <X>]
   project install-dir [<name|path>] [--branch <X>]
   project source-dir  [<name|path>] [--branch <X>]
   project work-dir    [<name|path>] [--branch <X>]
   project hash        [<name|path>] [--submodules none|direct|recursive] [--repos NAME]

``exists`` resolves a bare or fully-qualified name, then succeeds only when
``repos.main.path`` is the root of a Git working-tree checkout or bare clone.
A registered but un-cloned project returns a non-zero status; missing and
ambiguous names remain lookup errors.

The four ``*-dir`` queries resolve the same build plan as
``build``/``info`` and print one of its paths — ``build_dir``,
``install_dir``, ``source_dir``, or the selected repo's ``workdir``
respectively (``build-dir``/``install-dir`` error when the **build repo**
declares no such template; ``source-dir``/``work-dir`` are always resolvable).
The branch defaults to the anchored repo's current one. This is how a checkout
hook points ``compile_commands.json`` at the active build, or a script changes
directory into a branch's ``repos.<name>.workdir``.

``project hash`` prints a repo's commit for a branch — with ``--submodules``,
descending into pinned gitlinks read from the tree (never a checkout or branch
switch; the walk runs in the branch's own checkout and reports only the
submodules that are actually materialised). ``--repos NAME`` — repeatable
and/or comma-separated, and requiring ``--submodules direct|recursive`` — lets
a declared submodule repo's **live HEAD** override the pinned gitlink in the
output — the components you are actually working on.

Positionals, full
~~~~~~~~~~~~~~~~~

Each verb's positional is a **name or a path**, mutually exclusive:

* **path** if the token is ``.``/``..`` or begins with ``.``, ``/``, or ``~``.
  It may point *inside* a checkout; the owning project is found by
  deepest-prefix match (``project_for_path``). Shells expand ``~`` and leave
  ``./`` literal, so in practice the classifier keys on a leading ``.`` or
  ``/``.
* **name** otherwise — a bare name or a fully-qualified ``org/name``. A bare
  name ambiguous across organisations is a hard error asking you to qualify
  it. There is no ``--org`` flag.
* **omitted**: ``project`` covers every project; ``build``/``update`` operate
  on the project owning the current directory (a hard error if none does).

Global flags
~~~~~~~~~~~~

``-v/--verbose`` and ``-n/--dry-run`` are inherited from the ``wits`` process
layer and described in :doc:`/installation`.

Profile flags (affect resolution — ``build`` & ``project``)
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

These set the ``Profile`` axes and therefore change how paths
(``repos.<name>.workdir``, ``build_dir``, ``install_dir``) resolve.

.. list-table::
   :header-rows: 1
   :widths: 28 12 44 16

   * - Flag
     - Alias
     - Meaning
     - Default
   * - ``--branch <X>``
     - ``-b``
     - Target branch (the build identity).
     - focus repo's current branch
   * - ``--build-type <T>``
     - ``-B``
     - Build type (``debug``, ``release``, ``debugoptimized``, …).
     - ``debug`` (hardcoded; there is no config default)
   * - ``--toolchain <N>``
     - ``-T``
     - Select a declared toolchain.
     - selection chain
   * - ``--generator <G>``
     - ``-G``
     - Build-system generator (e.g. ``Ninja``).
     - the project's ``generator``
   * - ``--preset <P>``
     - ``-p``
     - Apply a preset; repeatable; accepts ``org/preset``.
     - —
   * - ``--focus <repo>``
     -
     - Override which repo is the build focus.
     - ``project.focus``
   * - ``--work-dir <DIR>``
     -
     - Use this checkout verbatim as the selected repo's ``workdir``,
       bypassing the branch strategy's ``worktree_dir``/in-place resolution.
       Everything (``build_dir``/``source_dir``/…) still anchors on it. The
       seam for building a checkout materialised elsewhere — a ``review
       checkout`` worktree.
     - strategy-resolved
   * - ``--spec <K=V>``
     -
     - Register a template variable, exposed as ``{{spec.K}}``; repeatable. A
       template that references ``{{spec.K}}`` **requires** it (hard error
       otherwise) — how an out-of-band value (an MR number, a variant tag)
       enters resolution without living in the file.
     - —

``--work-dir`` and ``--spec`` are ``Profile`` axes, so they work on the
``project`` read queries (``build-dir``, …) as well as ``build``.

Build-only flags
~~~~~~~~~~~~~~~~

.. list-table::
   :header-rows: 1
   :widths: 30 10 60

   * - Flag
     - Alias
     - Meaning
   * - ``--detach``
     -
     - Build the selected detached ``HEAD`` as-is, without a branch identity
       or checkout switch. Mutually exclusive with ``--branch``; an attached
       selected checkout is an error.
   * - ``--config-only``
     -
     - Configure only; do not compile.
   * - ``--build-only``
     -
     - Compile only; assume already configured (errors if not).
   * - ``--reconfig``
     -
     - Delete the build dir and configure fresh.
   * - ``--install``
     -
     - Add an install step after building.
   * - ``--install-dir <DIR>``
     -
     - Override the resolved ``install_dir`` prefix (the backend's
       install-prefix, e.g. cmake's ``CMAKE_INSTALL_PREFIX``). Affects
       configure as well as install.
   * - ``--build-dir <DIR>``
     -
     - Override the resolved ``build_dir``, ignoring the focus/anchor
       template — e.g. to build a ``review checkout`` in an isolated dir. The
       symmetric partner of ``--install-dir``; verbatim, highest priority.
   * - ``--uninstall``
     -
     - Reverse an install (backend-driven). Mutually exclusive with a build.
   * - ``--target <T>``
     - ``-t``
     - Build a specific target (where the backend supports it).
   * - ``--extra-config-args <A>…``
     - ``-Xconfig,<arg>``
     - Raw args appended to the configure command, verbatim.
   * - ``--extra-build-args <A>…``
     - ``-Xbuild,<arg>``
     - Raw args appended to the build command, verbatim.
   * - ``--extra-install-args <A>…``
     - ``-Xinstall,<arg>``
     - Raw args appended to the install command, verbatim.

Extra args are applied **last, at the highest priority**, and are never
interpreted by the tool.

Modes are mutually exclusive; the default is ``auto`` (configure if needed,
then build).

``project`` (describe / validate)
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

``project`` with no subcommand is the read command:

* No positional: a one-line summary of every project.
* A name or path: full details for that project, including each repo's
  branch/commit and any worktrees. With profile flags, resolved
  ``repos.<name>.workdir``/``build_dir``/``install_dir`` are shown; without
  them, the raw templates.
* ``--check``: validate configuration legality. No positional validates
  everything (CI use); a name/path validates one.

Worktrees are not here
~~~~~~~~~~~~~~~~~~~~~~

``project context {create,prune}`` is **removed**; :doc:`/commands/worktree`
manages worktrees for any repository. Nothing deletes a branch's
``build_dir`` any more — ``project build-dir`` prints the path.

Configuration topology
----------------------

* **Config root** (one only), resolved highest-first:
  ``$WITS_PROJECT_CONFIG`` → ``$XDG_CONFIG_HOME/wits/project`` →
  ``$HOME/.wits/project``.
* The root is scanned recursively for ``*.toml`` at load time. A file's
  top-level sections decide what it contributes; a file may mix sections.
* A file with ``[project]`` (and a required ``[repos.main]``) **is one
  project**. The same ``(org, name)`` in two files is a hard error.
* ``[toolchains.*]`` and ``[org]`` + ``[org.presets.*]`` are additive
  registries: distinct names accumulate across the whole tree, and the same
  name twice is a load-time error, not an override.
* Organisations are always explicit: ``[org] name = "…"`` declares one,
  ``project.org`` joins it. Never inferred from the file path.

Org config
~~~~~~~~~~

An org may declare shared value tables that every project joining it
inherits:

.. code-block:: toml

   [org]
   name = "acme"

   [org.environment]
   REGISTRY = "registry.acme.example"

   [org.definitions]
   ACME_VERSION = 3

These are **applied unconditionally** to every project with
``org = "acme"``, at pipeline layer L0.5 — below the project's own
``[project.environment]`` / ``[project.definitions]``, so a project overrides
an inherited value simply by declaring the same key:

.. code-block:: toml

   [project]
   org = "acme"
   [project.definitions]
   ACME_VERSION = 4          # wins over the org's 3

The rule is the same one that governs a project: a level's bare
``environment`` / ``definitions`` are that level's unconditional
contribution, while its ``presets`` apply only when named. Definitions keep
their TOML type through inheritance, so an org's ``false`` reaches a backend
as a boolean, not the string ``"false"``.

The same values are *also* exposed as ``org.environment.*`` /
``org.definitions.*`` in the template context — which is what you want when a
key must be bound under a different name, or in a context the build pipeline
never runs:

.. code-block:: toml

   [project.environment]
   PUSH_TO = "{{org.environment.REGISTRY}}"     # bind to a different name

   [repos.main.hooks]
   clone = "boot --at {{org.environment.REGISTRY}}"   # hooks: no pipeline, no L0.5

Naming an org that no file declares is not an error; it simply inherits
nothing (and any ``{{org.*}}`` reference then fails to resolve).

``[project]``
-------------

.. list-table::
   :header-rows: 1
   :widths: 18 14 22 46

   * - Key
     - Type
     - Required
     - Meaning
   * - ``org``
     - string
     - no
     - Organisation to join. Naming an org no file declares is not an error;
       it simply inherits nothing.
   * - ``focus``
     - string
     - no
     - Which ``[repos.*]`` is the build focus. Default ``"main"``.
   * - ``build_system``
     - string
     - when building
     - ``cmake`` \| ``meson`` \| ``cargo`` (backends shipping in v1).
   * - ``toolchain``
     - string
     - no
     - Default toolchain name (part of the selection chain).
   * - ``generator``
     - string
     - no
     - Build-system generator (e.g. ``Ninja``).
   * - ``default_presets``
     - list\<string\>
     - no
     - Presets always applied.

``[project.environment]`` and ``[project.definitions]`` — templated maps
merged at pipeline layer L1. ``environment`` becomes process env for the
build; ``definitions`` are build-system ``-D`` parameters.
``extra_config_args``, ``extra_build_args``, ``extra_install_args`` —
templated lists appended to the respective commands.

``[project.presets.<name>]`` — project-level presets.

Presets
-------

Declared at three levels:

* ``[org.presets.<name>]`` — org level (in a file that declares ``[org]``).
* ``[project.presets.<name>]`` — project level.
* ``[repos.<focus>.presets.<name>]`` — repo level (the focus repo).

Preset keys
~~~~~~~~~~~

.. list-table::
   :header-rows: 1
   :widths: 22 20 58

   * - Key
     - Type
     - Meaning
   * - ``extends``
     - string \| list
     - Inherit other presets; accepts ``org/preset``.
   * - ``applies_when``
     - table
     - Structured auto-application match.
   * - ``environment``
     - table
     - Templated env vars.
   * - ``definitions``
     - table
     - Templated build definitions.
   * - ``extra_config_args``
     - list
     - Appended to configure.
   * - ``extra_build_args``
     - list
     - Appended to build.
   * - ``extra_install_args``
     - list
     - Appended to install.

Cross-level merge
~~~~~~~~~~~~~~~~~

A referenced name is the merge of the same-named preset at each level:

* **Maps** (``environment``, ``definitions``): merged by key; on conflict the
  **nearest** (repo > project > org) level wins.
* **Lists** (``extra_*_args``): the **nearest** level's list **replaces** the
  others (not appended).

The merged definition's ``extends`` are then resolved.

``applies_when``
~~~~~~~~~~~~~~~~

A table over a fixed key set: ``build_type``, ``toolchain``, ``os``,
``arch``, ``generator``.

* Multiple keys are AND-ed.
* A key's value is a scalar (equality) or an array (membership / OR).
* Comparison is **case-sensitive**.

A match auto-applies the preset for that build.

Application order
~~~~~~~~~~~~~~~~~

``default_presets`` → ``applies_when`` matches → ``--preset`` (CLI). The
combined list is de-duplicated by name keeping the **last** position, so an
explicitly-passed preset moves late and wins. CLI ``-X``/``--extra-*-args``
(pipeline L3) sit above all presets.

``[toolchains.<name>]``
-----------------------

Toolchains are **100% user-declared** — there are no built-ins. The vocabulary
is aligned with meson's native file. All fields are optional; declare what
your build needs.

Canonical fields (translated to each backend)
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

.. list-table::
   :header-rows: 1
   :widths: 24 76

   * - Field
     - Meaning
   * - ``cc``, ``cxx``, ``rustc``
     - Compilers.
   * - ``ar``, ``nm``, ``ranlib``, ``strip``
     - Binutils.
   * - ``linker``
     - Linker (e.g. ``mold``, ``lld``).
   * - ``launcher``
     - Compiler launcher (e.g. ``ccache``, ``sccache``).
   * - ``c_flags``, ``cxx_flags``, ``link_flags``
     - Flag lists.
   * - ``supports``
     - Optional list of build systems, used only by ``info --check``.

Each canonical field is translated into a backend-native form; for meson and
cargo it is *also* exported as its universal environment variable (``CC``,
``CXX``, ``AR``, ``NM``, ``RANLIB``, ``STRIP``, ``CFLAGS``, ``CXXFLAGS``,
``LDFLAGS``, ``RUSTC``). cmake is the exception — see `Backends`_: it is
configured entirely through ``-D`` definitions and is deliberately given none
of these variables.

Pass-through blocks (not translated)
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

* ``[toolchains.<name>.environment]`` — env vars applied verbatim.
* ``[toolchains.<name>.definitions]`` — definitions applied verbatim.

Selection chain
~~~~~~~~~~~~~~~

::

   env (WITS_PROJECT_TOOLCHAIN)  >  --toolchain  >  the project's `toolchain` field

The resolved name is then looked up in the ``[toolchains]`` registry — an
unknown name is a hard error. The toolchain *name* is always selected (path
templates depend on it). Its
env/definitions **injection** is skipped in ``auto``/``build-only`` mode when
the build dir is already configured and no toolchain was explicitly requested.

Templates
---------

Config values are templated. Config format is TOML only.

``{{ … }}`` substitution
~~~~~~~~~~~~~~~~~~~~~~~~

The engine is Jinja (`MiniJinja
<https://docs.rs/minijinja/>`_), the same one the scaffold plugin generates
text with. Dotted lookup over the context (tables by attribute, arrays by
integer index). A value that is a single whole-string ``{{ … }}`` is evaluated
as an expression and returns the **typed** result (a list or integer
survives); anything else renders to a string. Resolution is lazy and recursive
with cycle detection.

A key holding a ``-`` must be subscripted, because ``repos.my-repo`` parses as
a subtraction::

   install_dir = "{{ repos['my-repo'].workdir }}"

Expressions
~~~~~~~~~~~

The full Jinja expression language, e.g.
``LINK_JOBS = "{{ [1, system.mem.gb // 4] | max }}"``.

* Operators: ``+ - * / // %`` over int/float; comparisons ``== != < <= > >=``;
  ``and``/``or``/``not``; the ``a if c else b`` ternary.
* Filters: Jinja's built-ins (``min``, ``max``, ``int``, ``float``, ``string``,
  ``join``, ``default``, …) plus ``prefix``, ``suffix``, ``strip_prefix``,
  ``pad`` and ``required``, and the ``fail('…')`` function.
* Statements (``{% if %}``, ``{% for %}``) work too, though a condition that
  selects a whole config layer belongs in ``applies_when`` rather than here.

Context variables
~~~~~~~~~~~~~~~~~

::

   project.{ name, org, focus }
   repo.*                     # the *current* repo (focus repo in project scope;
                              #   the repo itself in a repo-scoped field like a hook)
     { name, path, kind, main_branch, anchor, origin, upstream, mirrors }
   repos.<name>.*             # any repo by explicit name; same fields as repo.*
   org.environment.<K>        # org entry; inherited, and nameable here too
   org.definitions.<K>        # org entry; inherited, and nameable here too
   repos.<name>.workdir       # effective checkout dir for the named repo
   branch.{ raw, slug }       # attached builds only: raw branch + filesystem slug
   build_type
   toolchain.{ name, cc, cxx, rustc, ar, nm, ranlib, strip,
               linker, launcher, c_flags, cxx_flags, link_flags }
   generator
   system.{ os, arch, memory.gb, cpu.count }
   env.*                      # process environment
   spec.*                     # CLI-registered vars (--spec K=V); required if referenced

* ``repo`` is a **relative** alias for the repo being resolved; use
  ``repos.<name>`` to reference any other repo.
* There is no bare ``{{branch}}``; use ``{{branch.raw}}`` or
  ``{{branch.slug}}``.
* An explicit ``build --detach`` does not bind ``branch.*``. References
  therefore fail hard unless the corresponding path is replaced by
  ``--build-dir`` / ``--install-dir``.
* ``repo.upstream`` falls back to ``repo.origin`` when no upstream is
  declared.
* ``spec.*`` holds only what ``--spec K=V`` supplied on the command line, so
  a template referencing ``{{spec.mr}}`` fails loudly unless the caller
  passes it — never guessed or defaulted.
* ``org.environment.*`` / ``org.definitions.*`` are available in project
  scope and in repo-scoped fields (hooks, ``worktree_dir``,
  ``bootstrap_worktree_dir``). Only accessible when ``project.org`` is set and
  the org declares the key; references to undeclared keys are hard errors.
  They are **not** available in a ``repos.*.path`` template, which resolves
  against the Profile-free path context.

Errors
~~~~~~

Every failure is hard: unknown path, cycle, type mismatch, division by zero.
The context is always fully populated, so a missing path always means a real
mistake.

Backends
--------

``build_system`` selects a backend. A backend does three things: translates
the selected toolchain's canonical fields to native form, emits the command
steps for a mode, and detects prior configuration.

Canonical-field translation
~~~~~~~~~~~~~~~~~~~~~~~~~~~

.. list-table::
   :header-rows: 1
   :widths: 20 28 28 24

   * - Canonical
     - cmake
     - meson
     - cargo
   * - ``cc`` / ``cxx``
     - ``CMAKE_C/CXX_COMPILER``
     - ``CC``/``CXX`` env
     - ``CC``/``CXX`` env
   * - ``rustc``
     - —
     - —
     - ``RUSTC``
   * - ``ar``/``ranlib``/``strip``/``nm``
     - ``CMAKE_AR``/``CMAKE_RANLIB``/…
     - env
     - env
   * - ``linker``
     - ``-fuse-ld`` (appended to the linker flags)
     - ``CC_LD``/``CXX_LD``
     - —
   * - ``launcher``
     - ``CMAKE_*_COMPILER_LAUNCHER``
     - prefix on ``CC``/``CXX``
     - ``RUSTC_WRAPPER``
   * - ``c_flags``/``cxx_flags``/``link_flags``
     - ``CMAKE_C/CXX_FLAGS`` / linker flags
     - ``CFLAGS``/``CXXFLAGS``/``LDFLAGS``
     - ``CFLAGS``/``CXXFLAGS``/``LDFLAGS`` + ``RUSTFLAGS``

For meson and cargo, each canonical field is *also* exported as its universal
env var; **cmake is the exception** — it is configured entirely through ``-D``
definitions and is not given these environment variables, which it does not
need and which can conflict with its cached compiler. This translation runs
at pipeline layer L0, so an explicit preset or CLI override of the same key
wins.

Multi-config cmake generators (Ninja Multi-Config, Visual Studio, Xcode) are
handled correctly: ``CMAKE_BUILD_TYPE`` is *not* set at configure, and the
build type is selected at build/install time with ``--config``.

``is_configured``
~~~~~~~~~~~~~~~~~

* cmake: ``CMakeCache.txt`` present in the build dir.
* meson: ``meson-private/coredata.dat`` present.
* cargo: not applicable.

Modes
~~~~~

``auto`` \| ``config-only`` \| ``build-only`` \| ``reconfig`` \| ``uninstall``.
``--install`` adds an install step to a build. ``uninstall`` is
backend-driven — meson ``ninja -C <build> uninstall``, cmake via
``install_manifest.txt``, cargo unsupported — never a recursive delete,
because an install prefix may be shared.

``info --check`` validation
---------------------------

Reports (does not fix): a repo with its own git but no ``main_branch``;
``build_dir`` set while ``build_system`` is not; the project's ``toolchain``
naming an undeclared toolchain, or a toolchain whose ``supports`` list
contradicts the project's ``build_system``; preset inheritance and template
reference cycles; and template resolvability against a representative
context — one dry resolve at branch ``main``. For a cloned checkout, that a
declared ``skip`` is in force. No ``<name>`` checks every project.

Malformed structure is rejected earlier, when the registry loads, so
``--check`` never sees it: a file with no ``[repos.main]``, a repo with
neither ``path`` nor ``from``, a ``from`` naming an unknown project or repo,
a borrowed repo that is itself borrowed, and a travelling field declared
alongside ``from`` all fail the load outright. So do a worktree/hybrid repo
without ``worktree_dir``, a hybrid repo without ``bootstrap_worktree_dir``,
and a bootstrap template that references ``branch.*`` or resolves to nothing.

Whether the declared ``build_system`` actually has a backend is **not**
checked here — that is reported by ``wits build`` at run time, since the
read-only core deliberately knows nothing of which build systems are
implemented.

Repos, branches, and build contexts
-----------------------------------

``[repos.<name>]``
~~~~~~~~~~~~~~~~~~

.. list-table::
   :header-rows: 1
   :widths: 24 14 20 42

   * - Key
     - Type
     - Required
     - Meaning
   * - ``path``
     - **template**
     - yes, unless ``from``
     - On-disk repository location (the bare common dir for worktree/hybrid)
       or subpath relative to ``repos.main`` (nested). Resolved against a
       Profile-free context: ``project.name``, ``project.org``, ``env.*``,
       ``system.*`` — no ``repos.*`` (would be circular). Nesting +
       ``main_branch`` determine the inferred kind.
   * - ``from``
     - string
     - yes, unless ``path``
     - Borrow another project's repo as this one:
       ``[<org>/]<project>[:<repo>]``, the repo defaulting to ``main``. See
       below.
   * - ``skip``
     - list\<string\>
     - no
     - Paths this checkout never materialises: ordered gitignore-style
       patterns where ``!`` re-includes. **Not templated.**
   * - ``main_branch``
     - string
     - own-git repos
     - The branch ``update`` fast-forwards. Not allowed for ``subtree``.
   * - ``anchor``
     - string
     - no
     - Repo whose ``path`` is this build's source/base; unset → self.
   * - ``source_dir``
     - template
     - no
     - Where the backend configures from (the top-level
       ``CMakeLists.txt``/``meson.build``/…) when it is not the checkout
       root. Read from the **anchor** (or the focus when self-anchored);
       defaults to its ``repos.<name>.workdir``.
   * - ``build_dir``
     - template
     - when building
     - Where the backend writes its build tree. The focus's value overrides
       its anchor's default; CLI overrides both.
   * - ``install_dir``
     - template
     - no
     - Install prefix. Templated; the same focus-over-anchor precedence as
       ``build_dir``.
   * - ``branch_strategy``
     - string
     - no
     - ``in-place`` (default) \| ``worktree`` \| ``hybrid``. Worktree and
       hybrid use a bare clone.
   * - ``worktree_dir``
     - template
     - worktree/hybrid; optional otherwise
     - Where a worktree belongs. Attached worktree/hybrid builds use it for
       the target branch (hybrid first discovers an existing checkout).
       During ``build --detach``, a template resolvable without ``branch.*``
       is a fixed checkout selector even for an in-place entry; a
       branch-keyed template falls back to the caller's/primary checkout. A
       relative result is anchored beside ``repo.path``, never at process
       cwd.
   * - ``bootstrap_worktree_dir``
     - template
     - hybrid; optional for worktree
     - Fixed initial ``main_branch`` checkout created after a bare clone.
       Must not reference ``branch.*``. Worktree defaults to
       ``worktree_dir`` rendered for ``main_branch``; an explicit relative
       value is resolved beside that rendered main path.

**Kind is inferred, not declared**: a non-nested ``path`` → ``standalone``; a
nested ``path`` with ``main_branch`` → ``submodule``; nested without
``main_branch`` → ``subtree``. ``repos.main`` is always standalone.

Remotes
^^^^^^^

``[repos.<name>.remotes]`` — ``origin`` (string, the push target / fork),
``upstream`` (string, the **sync source**), ``mirrors`` (list of extra push
URLs on origin). The **sync source** = ``upstream`` if declared, else
``origin``; it is what ``clone`` and ``update`` fetch from and fast-forward
``main`` against. When an ``upstream`` is declared, ``origin`` is **never
fetched or cloned** — so a fork that does not yet exist on the server is fine
(it is only added as a push target). Reconciliation is additive only: missing
remotes/mirror push-URLs are added; existing URLs are never modified or
removed; unmentioned remotes are untouched.

Hooks
^^^^^

``[repos.<name>.hooks]`` — inline ``sh -c`` command strings, templated.
Phases: ``clone`` / ``post_clone`` and ``pre_update`` / ``update`` /
``post_update``. (The clone phase has no ``pre`` hook — the repo does not
exist yet.) The bare phase name (``clone``, ``update``) overrides that
phase's default action; ``pre_``/``post_`` add hooks around it.

**Hook cwd by phase**: a ``clone`` override runs in the **current working
directory** (the repo's ``path`` does not exist yet, and ``git clone``
creates the destination itself). For in-place, later hooks run in ``path``;
for worktree/hybrid, ``post_clone`` and update hooks run in the
bootstrap/current ``main_branch`` worktree when it exists, otherwise update
hooks fall back to the bare ``path``. A bare clone override must create its
configured bootstrap worktree.

A non-zero exit fails fast.

``[repos.<name>.presets.<preset>]`` — repo-level presets.

``{{repos.<name>.workdir}}`` resolution
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

``repos.<name>.workdir`` is the named repo's effective checkout. In a build,
the **anchor** repo's ``workdir`` is where sources come from; the branch
identity comes from the focus's nearest own-git repo (the focus itself, or its
anchor when the focus is a subtree) — and that identity repo's checkout is the
one a branch-backed build switches, or requires to already hold the branch.

Every repo resolves its own ``workdir`` by its own ``branch_strategy``, so the
strategies mix freely in one project. Anything that touches a working tree — a
branch switch, a merge, a submodule update, a sparse mask, reading the current
branch — runs in that ``workdir``, never in ``path``, which for a bare-backed
repo is a git-dir with no working tree at all.

* **in-place**: ``repos.<name>.workdir`` = that repo's ``path``. Switching
  the focus to a non-current branch stashes, switches, builds, then always
  restores (branch, stash, and the focus's submodules) on any exit. This is
  the only strategy that switches anything: a bare-backed repo holds each
  branch in a worktree already.
* **worktree**: ``repos.<name>.workdir`` = that repo's resolved
  ``worktree_dir`` for the target branch. It must already exist; ``build``
  never creates it. ``wits worktree create <branch> "$(project work-dir … --
  branch <branch>)"`` makes one.
* **hybrid**: if Git reports a live worktree currently attached to the target
  branch, its actual path wins regardless of location. Otherwise resolution
  returns ``worktree_dir`` as the suggested path; ``build`` then fails with
  the creation command rather than creating it.

For ``build --detach``, these branch rules are replaced by the
fixed-selector rule: a ``worktree_dir`` that resolves without ``branch.*``
wins regardless of strategy; otherwise the caller's checkout or the repo's
primary checkout wins.

Branch identity
~~~~~~~~~~~~~~~

The normal identity is the branch name of the nearest own-git repo in the
``focus → anchor`` chain. ``branch.slug`` replaces every character outside
``[A-Za-z0-9._-]`` (including ``/``) with ``_``.

``build --detach`` is the explicit branchless exception. It requires that
identity repo's selected checkout to have a real detached ``HEAD``, consumes
the selected checkout of each repo without switching, and exposes no
``branch.*`` variables. A declared ``worktree_dir`` that resolves without
``branch.*`` selects that repo's checkout first; otherwise resolution falls
back to the caller's or primary checkout. Without the flag, encountering a
detached ``HEAD`` remains a hard error naming ``--detach`` and ``--branch``.
``--work-dir`` only overrides the build repo's checkout location; it neither
implies nor replaces ``--detach``.

No single ``branch_strategy`` governs a build — each checkout answers for
itself. The **build repo's** strategy decides what ``build`` demands of the
checkout it sources from (an existing worktree for worktree/hybrid, nothing
for in-place). The **identity repo's** own strategy decides what happens to
the checkout carrying branch identity: an in-place identity is switched there
and back behind the restore guard; a bare-backed identity must already hold
the target branch in a worktree. A focus's own strategy still resolves its
``{{repos.<focus>.workdir}}`` binding — it moves nothing the build does not
act on. To point the build at a particular worktree of a focus, pass it with
``--work-dir``. ``source_dir`` is read from the anchor;
``build_dir``/``install_dir`` from the focus with anchor defaults (as tabulated
above).

``from`` — borrowing another project's repo
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

``from = "[<org>/]<project>[:<repo>]"`` makes this entry *be* another
project's repo. The project reference resolves exactly as a CLI positional
name does: bare or ``org/name``, ambiguity across orgs is an error. The repo
defaults to ``main``.

Resolved once at load. What the source supplies, and may therefore **not** be
declared alongside ``from`` (doing so is a hard error naming the fields):

::

   path   main_branch   branch_strategy   worktree_dir
   bootstrap_worktree_dir   skip   remotes   hooks

What stays the borrower's: ``anchor``, ``source_dir``, ``build_dir``,
``install_dir``, ``presets``.

That list is the complete local override surface. In particular, a source
repo's build/install paths do **not** travel: they describe building that
project, while the borrower may consume the same checkout through a different
anchor. A borrowed focus may declare local build/install paths; they override
the anchor defaults exactly like any other focus.

* The resolved ``path`` is the source's **absolute** path, so a nested source
  resolves against its own project's root.
* A borrowed repo's inferred kind is always ``standalone`` — from this
  project's side it is an external checkout with its own git.
* **A borrow may not itself be borrowed** (hard error).
* **A borrow never owns a path.** ``project_for_path`` / ``repo_for_path``
  ignore borrowed entries, so a checkout shared by several projects resolves
  to the one that declares it as its own.
* **``update`` skips borrowed repos** unless ``--with-borrowed`` is passed. A
  borrowed hook then resolves against the *borrower's* ``org.*`` namespace.

``skip`` — paths never checked out
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

An ordered list of gitignore-style patterns naming what to leave *out*; a
``!`` entry re-includes, and the **last matching entry wins**:

.. code-block:: toml

   skip = ["/third_party/engine", "/vendor", "!/vendor/keep.c"]

Realised as sparse-checkout patterns ``/*`` plus each entry with its leading
``!`` toggled, always ``--no-cone`` (cone mode cannot express an exclusion). A
skipped path that is a **submodule** additionally needs ``git submodule
deinit``, before the sparse write — sparse alone cannot remove a materialised
submodule.

.. list-table::
   :header-rows: 1
   :widths: 24 76

   * - When
     - What happens
   * - in-place clone
     - Patterns written **before** the checkout, so nothing skipped is ever
       materialised.
   * - worktree/hybrid clone
     - Create bootstrap checkout, write the patterns onto it (nothing skipped
       is materialised yet), then materialise submodules.
   * - clone override
     - Apply the mask to the checkout the override created, run
       ``post_clone``, then verify again.
   * - ``wits worktree create``
     - Driven from the primary/bootstrap checkout, so ``git worktree add``
       copies its pattern file.
   * - ``update``
     - **Verifies** before doing anything; a contradicted ``skip`` is a hard
       error.
   * - ``project --check``
     - Verifies and reports.

Writing the patterns is **refused** when the checkout already has sparse
patterns wits did not write — including any cone-mode configuration, which
cannot hold an exclusion at all. ``sparse-checkout set`` replaces the whole
list, so a checkout that something else (typically a ``clone`` hook) narrowed
to a cone would be *widened* rather than masked. Fold the exclusions into
those patterns where they are maintained, or do not declare ``skip`` for that
repo.

Verification is behavioural, not textual — it asks whether anything the list
excludes is still materialised, so extra sparse patterns of your own are legal
and ignored. It reports two things: a wholly-excluded path that exists on
disk, and an index entry under a skipped path not tagged ``S``
(skip-worktree). Glob entries, and paths under them, are **not** verified.
Under ``-v`` the git commands that would fix a violation are printed, in the
order that works; wits never runs them itself, because applying a mask to a
tree it did not build means deleting content.

``update`` / ``clone``
----------------------

For each repo (parents before nested; subtrees do no git work):

The **sync source** = ``upstream`` if declared, else ``origin``.

* **Missing path → clone**: in-place defaults to ``git clone`` from the sync
  source; worktree/hybrid build a **tracking bare host** — ``git init --bare``,
  ``git remote add``, ``git fetch --tags``, then ``main_branch`` created from
  ``<sync>/<main_branch>`` as the repository's symbolic HEAD — and add the
  configured bootstrap worktree on it. Deliberately not ``git clone --bare``,
  which copies every remote branch into ``refs/heads``, writes no fetch
  refspec, and publishes no ``origin/HEAD``. The mask lands before anything is
  materialised — in-place writes the patterns before its first checkout,
  bare-backed onto the bootstrap — then submodules are materialised,
  ``post_clone`` runs in the checkout, and ``skip`` is verified again. A
  ``clone`` override runs in the current directory, owns both repository and
  bootstrap creation, and gets the mask applied after the fact (deinit covered
  submodules, then write). Cloning names the fetched remote after the sync
  source, so tracking an ``upstream`` leaves ``origin`` free for a fork.
* **Existing → update**: ensure remotes (additive — including a fetch refspec
  for a remote that has none, which is how a repository cloned with
  ``git clone --bare`` is repaired) → ``pre_update`` → action →
  ``post_update`` (cwd = conventional checkout, bare main worktree, or bare
  path when that worktree is absent).

Default update action — how ``main`` advances turns on **whether any checkout
holds it**, not on whether the repository is bare:

* A checkout holds ``main_branch`` (the repository's own working tree, or the
  linked worktree holding it): ``git fetch <sync>`` then ``git merge --ff-only
  <sync>/<main_branch>`` there.
* Nothing holds it: after the same plain ``git fetch <sync>`` (the refspec
  ``ensure_remotes`` has just guaranteed makes it meaningful), advance the
  local branch ref with ``git update-ref``, refusing anything that is not a
  fast-forward. No working tree is touched and no sparse checkout is
  expanded; nested repo lifecycle work is skipped until a main worktree
  exists again.
* Declared submodule repos advance via their own lifecycle; undeclared nested
  submodules are refreshed with ``git submodule update --recursive -- <materialised
  paths>`` (no ``--init``; ``--init`` happens only on clone or worktree
  creation).

Failure is fail-fast: a non-zero hook/action stops the operation, an RAII
guard restores the original branch (and pops any stash), a log line is
written, remaining repos are skipped, and the process exits non-zero.

Crate API (read-only)
---------------------

The real surface, for a consumer written against it — ``wits build``,
``wits update``, and the ``wits project`` CLI are exactly such consumers:

.. code-block:: rust

   use wits_util::project::{resolve, resolve_target};
   use wits_util::project::workspace::Workspace;

   let ws = Workspace::load()?;                  // resolves the config root itself
   // Workspace::load_from(&root) reads one root outright.
   ws.projects();                                // iterate every ProjectData
   ws.project("mesa/lavapipe")?;                 // by bare name or org/name
   ws.project_for_path(&path);                   // which project owns this checkout
   ws.repo_for_path(&path);                      // …and which repo of it
   ws.toolchains();  ws.org_base(org);           // the shared registries

   let p: &ProjectData = ...;                    // fields: name, org, source, project, repos
   p.key();  p.focus_name(override);             // org/name; override → project.focus → "main"
   p.kind_of(name);  p.is_borrowed(name);        // inferred kind; from-borrow?
   p.repo_abs_path(name)?;                       // the rendered, ~-expanded path

   resolve::plan(&ws, p, &PlanInput::paths_only(&profile, branch))?;  // the full Plan
   resolve::resolve_target(&ws, target)?;        // name | path | cwd → project
   resolve::work_dir(&ws, p, repo, branch)?;     // a repo's checkout for a branch
   resolve::current_branch(&ws, p, repo)?;       // via current_checkout (cwd-aware)
   resolve::checkout_holding(&ws, p, repo, branch)?;
   resolve::primary_checkout(&ws, p, repo)?;
   resolve::repo_primary_path(&ws, p, repo)?;  resolve::nesting_root(&ws, p, repo)?;
   resolve::identity_repo(p, focus);  resolve::anchor_of(p, focus);

A ``Plan`` carries the whole resolution — ``focus``, ``build_repo``,
``identity_repo``, ``strategy``, ``branch``, ``build_type``, ``generator``,
``build_system``, ``toolchain``, ``work_dir``/``work_dirs``, ``source_dir``,
``build_dir``/``install_dir``, and the accumulated ``LogicalConfig`` — so a
caller reads rather than recomputes. The ``skip`` module (``violations``,
``remedy``, ``sparse_patterns``) is the other read-only piece.

The core resolves paths but never destroys them; the only side-effecting entry
points are the ``build`` and ``update`` actions.
