.. _wits-project:

``wits project``
================

Build, update, and introspect the source projects you work on, from one
declarative registry that knows *what each project is* — where its repos live,
which branches, which toolchain, how to build it — and drives cmake / meson /
cargo on your behalf without you re-typing the same flags.

Three commands share that registry: **``wits project``** describes and
validates (read-only), :doc:`build` configures and builds, and :doc:`update`
refreshes git. Per-branch worktrees belong to :doc:`worktree`, which is
project-agnostic. This page is the **usage guide** for the registry and for
``project`` itself.

For the exhaustive list of every config key and every flag, see
:doc:`/reference/project-reference`. For *why* the tool is shaped this way, see
:doc:`/reference/project-design`.

The mental model in one minute
------------------------------

* A **project** is a buildable unit described by one TOML file. It owns one or
  more **repos** (git checkouts); one of them, ``repos.main``, is always
  required. A repo may instead be **borrowed** from another project with
  ``from``, which is how a component several projects consume gets a single
  home.
* A **toolchain** is a named set of compilers/tools you declare once and select
  per build. The tool ships no built-in toolchains — you declare your own.
* A **preset** is a reusable bundle of build settings you can layer on.
* A **build context** is where a branch actually builds: the git *worktree*
  holding that branch, or the repo's own in-place checkout — your choice per
  repo, with the build directory resolved beside either.
* An **org** is a project's shared parent. Its ``[org.environment]`` /
  ``[org.definitions]`` are inherited unconditionally by every project that
  joins it; its ``[org.presets.*]`` apply only when named.

Everything is content-addressed: config files can live anywhere under the
config root and declare what they are by their sections. There is no required
layout.

``repos.main.path`` (and any repo's ``path``) is a template resolved against
``project.name``, ``project.org``, ``env.*``, and ``system.*`` — so paths like
``~/src/{{project.org}}/{{project.name}}`` work and remain answerable without a
full build profile.

Where config lives
------------------

``project`` reads one config root, resolved in this order:

1. ``$WITS_PROJECT_CONFIG`` (environment)
2. ``$XDG_CONFIG_HOME/wits/project``
3. ``$HOME/.wits/project``

Drop ``*.toml`` files anywhere under it. A file with a ``[project]`` section is
a project; files with ``[toolchains.*]`` or ``[org]`` sections contribute to
shared registries — distinct names accumulate across the tree, and the same
name twice is a load-time error, not an override.

Your first project
------------------

Create ``~/.wits/project/hello.toml``:

.. code-block:: toml

   [project]
   build_system = "cmake"
   toolchain    = "clang"

   [repos.main]
   path        = "~/src/hello"
   main_branch = "main"
   # Where the build lands. `repos.main.workdir` is this build's checkout dir;
   # keying by branch means switching branches never clobbers another build.
   build_dir   = "{{repos.main.workdir}}/_build/{{toolchain.name}}/{{build_type}}"
   install_dir = "{{repos.main.workdir}}/_install/{{build_type}}"
   [repos.main.remotes]
   origin = "git@github.com:me/hello.git"

Declare the ``clang`` toolchain once (e.g. ``~/.wits/project/toolchains.toml``):

.. code-block:: toml

   [toolchains.clang]
   cc       = "clang"
   cxx      = "clang++"
   ar       = "llvm-ar"
   linker   = "mold"
   launcher = "ccache"
   supports = ["cmake", "meson"]

Now:

.. code-block:: sh

   update  hello      # clone if missing, otherwise refresh git
   build   hello      # configure + build with clang, debug by default
   project hello      # what is it, where does it build, what branch is it on

``build`` translates the toolchain into cmake's native flags for you — you
never write ``CMAKE_C_COMPILER`` yourself.

Building: types, toolchains, presets, modes
-------------------------------------------

.. code-block:: sh

   build hello -B release                 # build type (lowercase, meson-aligned)
   build hello -T gcc                     # a different declared toolchain
   build hello -p asan -p lto             # apply presets (repeatable)
   build hello --config-only              # (re)configure, don't compile
   build hello --build-only               # compile, assume already configured
   build hello --reconfig                 # wipe the build dir and configure fresh
   build hello --install                  # add an install step
   build hello --uninstall                # reverse an install (backend-driven)

The full flag surface for ``build`` is its own chapter: :doc:`build`.

Choosing a toolchain without editing config
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

Selection order is ``env (WITS_PROJECT_TOOLCHAIN) → --toolchain → the
project's toolchain field``; the resolved name is then looked up in
``[toolchains]``. Environment wins, so you can flip toolchain for a run:

.. code-block:: sh

   WITS_PROJECT_TOOLCHAIN=gcc build hello

(Selecting a toolchain always happens so paths like
``.../{{toolchain.name}}/...`` resolve; in ``auto``/``build-only`` mode an
already-configured build dir is trusted and not reconfigured just because you
re-ran ``build``.)

Presets
-------

A preset bundles environment, definitions, and extra args, and can inherit and
auto-apply itself:

.. code-block:: toml

   [project.presets.debug]
   definitions = { ENABLE_ASSERTS = true, ENABLE_TESTS = true }

   [project.presets.asan]
   extends      = ["debug"]
   applies_when = { build_type = "debug", toolchain = ["clang", "clang-cl"] }
   environment  = { ASAN_OPTIONS = "detect_leaks=1" }
   definitions  = { SANITIZER = "address" }

   # A name in default_presets must resolve — an undefined preset is a hard
   # error on any resolve.
   [project.presets.warnings]
   extra_config_args = ["-DCMAKE_C_FLAGS=-Wall -Wextra",
                        "-DCMAKE_CXX_FLAGS=-Wall -Wextra"]

   [project]
   default_presets = ["warnings"]     # always applied

* ``default_presets`` always apply; an ``applies_when`` match auto-applies;
  ``-p NAME`` applies explicitly. The combined list is de-duplicated by name
  keeping the **last** position, so an explicitly-passed preset is applied
  late — presets do not override each other by source; later layers simply
  apply later.
* Presets exist at three levels — ``[org.presets.*]``, ``[project.presets.*]``,
  ``[repos.<focus>.presets.*]`` — and a name is the merge of all three, most
  specific winning. Reach another org with ``-p llvm/base``.

See the reference for the exact merge and match rules.

Multiple repos: monorepos, submodules, subtrees
-----------------------------------------------

A project can own several repos. ``repos.main`` is the required root; others
hang off it and pick a ``focus`` for building.

.. code-block:: toml

   [project]
   focus        = "lvp"           # build the lavapipe component
   build_system = "meson"
   toolchain    = "clang"

   [repos.main]                   # the mesa clone (required root)
   path        = "~/src/mesa"
   main_branch = "main"
   build_dir   = "{{repos.main.workdir}}/_build/lvp/{{build_type}}"
   [repos.main.remotes]
   origin   = "git@github.com:me/mesa.git"
   upstream = "https://gitlab.freedesktop.org/mesa/mesa.git"

   [repos.lvp]                    # a subtree (inferred: nested path, no main_branch)
   path   = "src/gallium/frontends/lavapipe"   # relative to repos.main → shares mesa's git
   anchor = "main"                # build via the mesa root

* ``anchor = "main"`` means "build from the mesa root" — the configure source is
  mesa, and lavapipe is selected through meson options. ``anchor`` may point at
  any repo, or be left unset to build a repo on its own.
* The anchor supplies default ``build_dir``/``install_dir`` paths. A focus may
  override just those two fields while continuing to configure from the anchor;
  CLI path overrides remain highest priority.
* The **kind** of each repo is inferred, never declared: a nested path with its
  own ``main_branch`` is a **submodule**; a nested path without one is a
  **subtree**; a non-nested path is **standalone**. A submodule is cloned
  through ``repos.main``.
* ``update`` refreshes *every* repo; ``build`` builds the ``focus``; you can
  switch focus for one run with ``--focus <repo>`` — handy in a large monorepo.
* If a repo's top-level ``CMakeLists.txt``/``meson.build`` is not at the
  checkout root, point ``source_dir`` at it (a template, default: the build
  repo's own resolved workdir):
  ``source_dir = "{{repos.main.workdir}}/src"``. Only the configure source
  moves; ``repos.main.workdir``, that repo's ``build_dir``, and the branch
  still key off the checkout.

One component, several projects
-------------------------------

Sooner or later a component is a submodule of *more than one* project, at a
different path in each. Declared the obvious way that means the same repo
described in every project file, cloned once per project, and the same feature
branch created over and over. Two keys collapse it: **``from``** declares the
component once where it lives, and **``skip``** keeps each consumer's own copy
out of the way.

.. code-block:: toml

   # engine.toml — the component. A project of its own: one checkout, one update.
   [project]
   build_system = "cmake"

   [repos.main]
   path        = "~/src/engine"
   main_branch = "stable"
   [repos.main.remotes]
   origin = "git@github.com:me/engine.git"

.. code-block:: toml

   # viewer.toml — a project that consumes it.
   [project]
   focus     = "engine"          # the component is what you are working on

   [repos.main]
   path        = "~/src/viewer"
   main_branch = "main"
   skip        = ["/third_party/engine"]   # don't check out our own copy
   build_dir   = "{{repos.main.workdir}}/_build/{{branch.slug}}-{{build_type}}"

   [repos.engine]
   from   = "engine"             # …use that one instead
   anchor = "main"               # and build it through the viewer root

   # Point the build at the borrowed checkout. `{{repos.<name>.workdir}}` is
   # wits' variable for "the branch-specific working path of that repo"; the
   # definition's *name* (ENGINE_SOURCE_DIR here) is whatever your build
   # system consumes.
   [project.definitions]
   ENGINE_SOURCE_DIR = "{{repos.engine.workdir}}"

``build viewer`` now builds *your* engine checkout through the viewer, with
the build directory keyed by the engine's branch — and a second consumer
(``editor``, say) is another file with its own ``skip`` and its own
definitions, not a second copy of the engine.

``from``
~~~~~~~~

``from = "[<org>/]<project>[:<repo>]"`` (repo defaults to ``main``) makes the
entry *be* that project's repo. The rule for what comes along is one sentence:
**a repo's git identity travels; how this build uses it does not.** So ``path``,
``main_branch``, ``remotes``, ``hooks``, ``branch_strategy``, ``worktree_dir``,
``bootstrap_worktree_dir``, and ``skip`` come from the source — declaring one
of them here too is an error — while ``anchor``, ``source_dir``, ``build_dir``,
``install_dir``, and ``presets`` are yours, which is what lets each consumer
set its own build knobs (including output paths) on the same component.

Two things follow that are worth knowing before you hit them:

* **The owner answers for the path.** ``cd ~/src/engine && project`` gives you
  ``engine``, never a project that merely borrows it. Borrowed entries are not
  candidates for the path lookup, so a shared checkout has exactly one owner.
* **``update`` leaves borrowed repos alone**, so the component is fetched once
  by its owner rather than once per consumer. ``update viewer --with-borrowed``
  opts in.

``from`` can also name a nested repo — ``from = "viewer:engine"`` — which says
"the component lives inside *that* project's tree". Useful when you would
rather not move it out at all.

``skip``
~~~~~~~~

``skip`` lists the paths a checkout never materialises, as ordered
gitignore-style patterns where ``!`` re-includes and the last match wins:

.. code-block:: toml

   skip = ["/third_party/engine", "/vendor", "!/vendor/keep.c"]

It is not only for borrowing — a monorepo you build one component of is the
same need. It is realised as a non-cone sparse-checkout, plus ``git submodule
deinit`` for any submodule it covers (sparse alone cannot remove a materialised
submodule).

``skip`` belongs to the **repo**, not to a branch strategy: declare it on an
in-place shell, on a bare-backed component, or on both in one project, and it
is applied and verified the same way. What differs is only the mechanics of
getting the mask in place before anything materialises.

**``clone`` applies it; everything else only checks it.** An in-place clone
writes the patterns before its first checkout. A worktree/hybrid clone first
creates a bare repository and its bootstrap worktree, then applies the mask to
that bootstrap *before* initialising any submodule in it — a fresh bare host
has no checkout for ``git worktree add`` to copy patterns from, so masking
afterwards would mean cloning a skipped submodule in full and only then
deinitialising it. Later ``wits worktree create`` calls are driven from that
bootstrap checkout, so Git copies the mask.

``update`` and ``project check`` only *verify*, in that repo's own checkout
— the in-place clone itself, or for a bare-backed repo the worktree holding
``main_branch``, falling back to whichever checkout stands in for its missing
main worktree. Both look at the same checkout, so a ``--check`` that passes is
a promise ``update`` will not then contradict. Neither *applies* the mask:
doing that to a tree wits did not build means deleting content, which is yours
to do:

.. code-block:: sh

   update viewer
   # [ERROR] repo 'main': skipped path 'third_party/engine' is materialised …
   # [INFO] re-run with -v to see the commands that fix this

   update viewer -v          # prints the exact deinit + sparse-checkout commands

Your own sparse patterns are safe in a different sense: the check asks whether
anything the list excludes is still materialised, not whether the pattern file
matches what wits would have written. But wits will **refuse** to *write* over
patterns it did not put there — ``sparse-checkout set`` replaces the whole
list, so a checkout something else narrowed to a sparse cone would be widened
rather than masked. If a ``clone`` hook establishes its own sparse cone, fold
the exclusions into it there instead of declaring ``skip``.

Branches: in-place, worktree, and hybrid
----------------------------------------

Each repo picks how multi-branch work is realised — **per repo**, so an
in-place shell and a bare-backed component it borrows sit happily in one
project:

.. code-block:: toml

   [repos.main]
   # Choose one: "in-place" (default), "worktree", or "hybrid".
   branch_strategy        = "hybrid"
   worktree_dir           = "{{repo.path}}.worktrees/{{branch.slug}}"
   bootstrap_worktree_dir = "{{repo.path}}.primary"

* **in-place**: ``build --branch X`` stashes, switches to X, builds, then
  always switches back and restores your working tree — even if the build
  fails. This is the only strategy that switches anything, and it switches
  only the repo carrying branch identity: where a project borrows that
  identity from a bare-backed component, the branch already sits in one of its
  worktrees.
* **worktree**: ``path`` is a bare clone and each branch resolves
  deterministically to ``worktree_dir``. ``bootstrap_worktree_dir`` is
  optional; when absent, the initial main checkout is ``worktree_dir`` rendered
  for ``main_branch``.
* **hybrid**: also bare-backed, but first asks Git whether the branch is
  already checked out anywhere and uses that actual path. If not,
  ``worktree_dir`` is the suggested location. Its fixed
  ``bootstrap_worktree_dir`` is required and may not reference ``branch.*``. A
  relative bootstrap value such as ``"main"`` is resolved beside
  ``worktree_dir(main_branch)``, never against the command's current
  directory.

Both worktree modes require ``worktree_dir``. ``build`` requires the target
worktree to exist and never creates one implicitly:

.. code-block:: sh

   wits worktree create feature-x \
       "$(project info --get repo.workdir hello --branch feature-x)"
   build hello --branch feature-x                    # build in it
   wits worktree prune feature-x                     # reclaim it when done

Creating and reclaiming worktrees belongs to :doc:`worktree`, which does it
for **any** repository rather than only a registered one. ``project`` and
worktrees meet at a path: ask ``project info --get
``repo.workdir`` where the
strategy says the checkout goes, or skip the registry entirely and point
``build --work-dir`` at a worktree you made yourself.

One rule underpins all of it: ``repos.<name>.path`` is the **repository** (a
git-dir, with no working tree at all when the repo is bare-backed) and
``repos.<name>.workdir`` is a **checkout**. Anything that touches a working
tree runs in the latter — including reading "the current branch", which for a
bare-backed repo comes from a checkout and never from the bare repository's own
``HEAD``. Standing inside one of a repo's worktrees makes *that* the branch a
bare ``wits build`` is for; from anywhere else it is the branch its primary
checkout is on.

On a fresh ``update``, worktree/hybrid build a bare repository that **tracks**
its remote (``init --bare`` + ``remote add`` + ``fetch``, so the remote's
branches stay in ``refs/remotes/origin/*`` rather than being copied into
``refs/heads``), create the main bootstrap checkout, initialise its
submodules, and apply ``skip`` there. Existing conventional clones are not
converted.

.. note::

   Removing a worktree does not remove its **build directory**; ``wits
   worktree`` is project-agnostic and knows nothing about build dirs. Delete
   it yourself when you want the space back — ``project info --get build_dir
   hello --branch feature-x`` prints the path.

Updating
--------

:doc:`update` is covered in its own chapter; the essentials:

.. code-block:: sh

   update hello                    # one project's repos
   update                          # every project
   update hello --with-borrowed    # include repos owned by another project

``update`` is safe by default: if you are on a feature branch, it fast-forwards
the main branch's ref *without* checking it out — nothing is stashed or
switched, and a sparse checkout is never expanded. It also brings the remotes
you declared into line with the file — creating what is missing and correcting
what has drifted — while leaving any remote your config does not mention
completely alone, and fetches every one of them so the extras stay usable. Only
the merge target's fetch can fail the run. Repos borrowed with ``from`` are left
to the project that owns them unless you ask for them, and a repo whose declared
``skip`` is not in force is a hard error rather than a refresh.

Inspecting and validating
-------------------------

.. code-block:: sh

   project list                  # one-line summary of every project
   project info hello            # everything known about one
   project info hello -b feature-x -B release   # …resolved for that profile
   project check                 # validate every project's config (CI)
   project check hello           # validate one

``project`` is pure read — it never builds or switches anything.

``project info`` prints four sections: what the project is, its repos with
their git state, the path **templates** it declares, and the **resolution** of
those templates for one branch (plus the presets that applied and the
definitions/environment they produced). Templates and resolution are both shown
because they answer different questions, and a mismatch between them is usually
the bug you opened ``info`` to find. Resolution needs a branch, so that is the
one section that degrades — without ``-b``, and outside a checkout whose branch
can be discovered, it says so rather than going quiet.

Machine-readable queries for scripts and git hooks
--------------------------------------------------

``info --get <path>`` prints one resolved value per line instead of the
description. The path is a dotted lookup into **the same template context the
config files are written against**, so there is no second vocabulary to learn:
what ``{{build_dir}}`` or ``{{repos.main.workdir}}`` means in a ``[repos.*]``
table is what it means here.

.. code-block:: sh

   project info --get build_dir hello -b feature-x
   project info --get install_dir hello -b feature-x
   project info --get source_dir hello -b feature-x
   project info --get repo.workdir hello -b feature-x   # the focus's checkout
   project info --get repo.main_branch                  # of the repo you're in
   project info --get toolchain.linker --get build_type hello

``--get`` is repeatable, and each path prints one line in the order given — so a
script reads several answers out of **one** resolve rather than paying a registry
load per question. Two rules keep that safe to read back positionally: the value
must be a **scalar** (a table or a list is refused, with the description as the
place to look instead), and one failing path fails the whole command rather than
emitting a blank line you could not tell from an empty value.

The other three queries stay their own verbs, because each has a shape ``--get``
does not. ``exists`` answers with an **exit status** and works on a project that
is not cloned, so it never resolves a plan at all; ``branch-build-dirs`` returns
0..N rows spanning *other* projects; ``hash`` carries its own flags and walks git
objects rather than resolving config.

.. code-block:: sh

   project exists hello                  # exit 0 when hello's main repo is cloned
   project branch-build-dirs hello -b feature-x
   project hash hello --submodules recursive --repos <submodule>

``project hash`` reads a repo's commit hash for a branch — and, with
``--submodules``, the pinned hashes below it — straight from the tree, so no
checkout or branch switch is needed. ``--repos NAME`` lets a declared submodule
repo's **live HEAD** override the stale gitlink the superproject records: the
components you are actually working on, which a build takes at their
checked-out commit rather than the pinned one. ``NAME`` must be a declared
**submodule** (a standalone or subtree repo has no gitlink to override),
requires ``--submodules direct|recursive``, and may be repeated and/or
comma-separated. Only submodules that are actually checked out are reported.

``project branch-build-dirs`` is the reverse of ``--get build_dir``: given a
checkout and a branch it prints every **existing** build directory that branch
identifies, across all projects, as ``<project>\t<path>`` per line. The
cross-project shape is the point — a borrowed component can be the build
identity of several projects at once, so the single answer ``--get build_dir``
gives would leave every borrower's tree behind when the branch goes away. A project
that borrows the checkout but focuses elsewhere builds under its *own* branch and
is left out, as is a build directory whose path does not vary by branch; both are
derived from the registry rather than declared in it. It prints and removes
nothing — ``project`` is read-only, and the caller that deletes wants its own
confirmation.

These exist to be consumed: a checkout hook points ``compile_commands.json``
at the active build, a script changes into a branch's ``repo.workdir``, a
cleanup hook asks which build trees a deleted branch orphaned.

Running from inside a checkout
------------------------------

Every verb takes a project **name** or a **path**, or nothing at all. A path
may point *inside* a checkout — the owning project is found automatically —
and with no argument the verbs act on the project you are currently standing
in:

.. code-block:: sh

   cd ~/src/mesa/src/gallium/frontends/lavapipe
   build                # builds the project owning this directory
   project .            # details for that project
   project ~/src/hello  # by path, from anywhere

A token starting with ``.``, ``/``, or ``~`` is treated as a path; anything
else is a name (``hello`` or ``mesa/lavapipe``). With no argument, ``project``
lists every project while the other verbs use the current directory.

A checkout that several projects share resolves to the project that **owns** it —
the one declaring it as its own repo, rather than any that only ``from``-borrow
it. That is the right default while exactly one project builds the component, and
it is unambiguous: a borrow always points at an owner, so there is never a tie.

When the component is actually developed as one of its borrowers, its **owner**
says so, in the repo the borrowers point at:

.. code-block:: toml

   # engine.toml — the project that owns the checkout
   [repos.main]
   path = "~/src/engine"
   developed_as = "viewer"

Every path query then answers for ``viewer``, borrow and all — so a component
whose branch identifies the borrower's build resolves to the borrower's build
directory instead of its owner's. The owner declares it because the borrowers
cannot: several projects may borrow one component and none of them has grounds
to prefer itself, while the owner is unique. Being registry config it also
travels, so a fresh clone resolves correctly on its first checkout rather than
after you remember to set something locally.

A value naming no known project, or one that does not borrow the repo, is
ignored rather than fatal, so a registry deployed without the project it names
degrades to the ordinary answer instead of breaking every query. ``check``
reports both.

Global flags
------------

Inherited from ``wits`` (see :doc:`/installation`):

.. list-table::
   :widths: 24 76
   :header-rows: 1

   * - Flag
     - Meaning
   * - ``-v``, ``--verbose``
     - Show the underlying git / build commands as they run
   * - ``-n``, ``--dry-run``
     - Print mutating commands instead of running them (reads still run)
