.. _wits-gputest:

``wits gputest``
================

Run a GPU test suite against a wits build, compare the result with the previous
build, and keep one self-describing archive.

.. code-block:: sh

   wits gputest run radv-vk --if-changed   # the nightly question
   wits gputest run radv-vk                # always run, compare with the last
   wits gputest plan radv-vk               # why is it loading that driver?
   wits gputest install deqp-vk            # stage the suite and its runner
   wits gputest history radv-vk            # one line per archive

It answers one question:

   Given the driver build this machine has today and the test suite that is
   installed, did anything regress since the last build — and what is the
   evidence?

It does not build the driver, install it, schedule anything, talk to a forge,
or define a new output format. Builds belong to :doc:`build` and the nightly
service; the runtime environment belongs to :doc:`devenv`; suite installation
belongs to the suite project's own ``install`` hook. ``gputest`` owns the run,
the comparison, and the record.

The four nouns
--------------

.. list-table::
   :header-rows: 1
   :widths: 16 84

   * - Noun
     - Meaning
   * - **driver**
     - A wits project plus a profile plus an ICD selector. It is a build, not a
       prefix: its environment is whatever ``devenv`` composes, and ``select``
       chooses which ICD inside that environment the test loads.
   * - **suite**
     - One runner invocation: a kind, the test project whose ``install_dir`` is
       the suite root, the case list, and the runner arguments.
   * - **case**
     - A named ``driver × suite`` plus optional environment, and the unit of
       history, identity, and baseline.
   * - **run**
     - One execution: an id, a directory under the runtime dir, one archive in
       the store, and one state update when it completes.

The runner's individual entries are **tests**, not cases; a caselist names
tests, and the case names the comparison.

Configuration
-------------

The config is a tree of ``*.toml`` files under one root:
``$WITS_GPUTEST_CONFIG``, else ``$XDG_CONFIG_HOME/wits/gputest``, else
``$HOME/.wits/gputest``. Files may sit anywhere under it; a section decides what
a file contributes.

``[store]``, ``[driver.*]``, ``[suite.*]`` and ``[case.*]`` accumulate across
the tree. The same name twice is a load-time error, not an override; an unknown
section or key is a load-time error too. A test tool cannot afford a typo that
silently does nothing.

.. code-block:: toml

   [store]
   root = "~/Public/results"      # <root>/<case>/<device>/<id>.tar.zst
   archive_days = 360

   [driver.radv]
   project = "mesa"               # a wits project by registry name
   select = "*radeon*"            # VK_LOADER_DRIVERS_SELECT
   # build_type = "release"       # the default
   # branch = "main"              # default: the resolved focus's identity-repo main_branch

   [driver.lvp]
   project = "mesa"
   select = "*lvp*"

   [driver.amdvlk]
   project = "amdvlk"

   [suite.deqp-vk]
   kind = "deqp"
   project = "deqp"               # suite root = its install_dir
   case_list = ["mustpass/vk-default.txt"]
   jobs = "cores"                 # physical cores; or a number
   timeout = 300

   [suite.deqp-vk.deqp]
   exe = "deqp-vk"
   waiver = "waivers/vk.xml"
   args = [
       "--deqp-log-images=disable",
       "--deqp-log-shader-sources=disable",
       "--deqp-log-decompiled-spirv=disable",
       "--deqp-shadercache=disable",
   ]
   tests_per_group = 4096

   [suite.piglit]
   kind = "piglit"
   project = "piglit"
   case_list = ["quick"]          # exactly one profile per invocation
   jobs = 4
   timeout = 300

   [suite.piglit.piglit]
   folder = "lib64/piglit"        # the CMake-installed data directory

   [suite.piglit.env]
   PIGLIT_PLATFORM = "gbm"
   PYTHONDONTWRITEBYTECODE = "1"

   [case.radv-vk]
   driver = "radv"
   suite = "deqp-vk"

   [case.radv-zink-gl]
   driver = "radv"
   suite = "piglit"
   env = { MESA_LOADER_DRIVER_OVERRIDE = "zink" }

.. list-table::
   :header-rows: 1
   :widths: 22 78

   * - Section
     - What it says
   * - ``[store]``
     - ``root`` is required and absolute (``~`` is expanded). ``archive_days``
       defaults to 360 and is what ``prune`` reads. The store is per machine:
       state and baselines are local.
   * - ``[driver.*]``
     - ``project`` is required. ``build_type`` defaults to ``release``;
       ``branch`` defaults to the resolved focus's identity-repo
       ``main_branch`` (the focus repo when it has its own git, else its
       anchor). ``select`` is
       set as ``VK_LOADER_DRIVERS_SELECT``. ``env`` is a ``devenv``
       declaration table applied after ``devenv``.
   * - ``[suite.*]``
     - ``kind`` and ``project`` are required. ``runner`` defaults to
       ``runner``. ``case_list`` is interpreted by the kind. ``jobs`` defaults
       to physical cores; ``timeout`` to 60 seconds. ``env`` is applied after
       the driver.
   * - ``[suite.<name>.deqp]``
     - ``exe`` is required; ``waiver``, ``args`` and ``tests_per_group`` are
       optional. ``deqp`` accepts any number of caselist files and passes one
       ``--caselist`` per entry.
   * - ``[suite.<name>.piglit]``
     - ``folder`` is required: the directory holding ``tests/``, ``bin/`` and
       ``lib/``. Piglit takes exactly one profile per invocation.
   * - ``[case.*]``
     - ``driver`` and ``suite`` are required; ``env`` is applied last. The case
       name is a path component and must match ``[A-Za-z0-9._-]+``.

Every suite path (``exe``, ``waiver``, each caselist, ``folder``) is relative
to the suite root and must not escape it: absolute paths and components that
climb out with ``..`` are load-time errors. The kind's sub-table is the only
place its keys live; a ``deqp`` key in a ``piglit`` table is an unknown key.
``kind`` must name exactly one sub-table.

There is no ``enabled`` flag and no schedule. Which cases run nightly is a
service declaration; the config declares what a case *is*, not when it runs.

Kinds
-----

A kind is one runner shape, not one API. Both kinds use a binary from the
``deqp-runner`` project and the same evidence contract: ``results.csv``,
``failures.csv``, ``failures-all.csv``, ``expectations.json``, and exit
``0``/``1``/``2``.

* ``deqp`` — dEQP Vulkan through ``deqp-runner``. The installed tree needs the
  dEQP executable, ``vulkan/``, the caselists and the waiver. Its driver guard
  requires exactly one selected Vulkan ICD.
* ``piglit`` — Piglit GL through ``piglit-runner``. The installed tree is the
  CMake install output (``<folder>/{tests,bin,lib}``). Its driver guard is
  empty; a case selects zink with ``env``.

Adding a kind is a code change, not a config value: the ``Kind`` variant and
its ``SuiteShape`` variant, the sub-table, and
``runner_bin``/``argv``/``check_assets``/``check_driver`` on ``SuiteShape``.
The core pipeline never branches on the kind itself.

Environment
-----------

A run's environment is built in one direction and no layer is revisited:

#. The caller's environment, scrubbed of graphics and loader state: ``VK_*``,
   ``MESA_*``, ``LIBGL_*``, ``EGL_*``, ``__EGL_*``, ``GLX_*``, ``__GLX_*``,
   ``RADV_*``, ``R600_*``, ``RADEON_*``, ``ZINK_*``, ``NIR_*``, ``RUSTICL_*``,
   ``OCL_ICD_*``, ``GBM_*``, ``AMD_*``, ``AMDVLK_*``, ``LIBVA_*``,
   ``VDPAU_*``, plus ``LD_LIBRARY_PATH``, ``LD_PRELOAD``, ``LD_AUDIT``,
   ``GALLIUM_DRIVER``, ``DRIRC_CONFIGDIR``, ``DRI_PRIME``,
   ``DRI_DEVICE_NAME`` and ``WITS_DEVENV``. ``PATH`` is not scrubbed.
#. ``devenv``'s overlay: the build system's base (Meson's own ``meson devenv``)
   plus the project's declared ``devenv`` operations.
#. ``driver.select``, then the driver's ``env`` table.
#. The suite's ``env`` table.
#. The case's ``env`` table.

The three test-only tables use the same vocabulary as the project registry's
``devenv``: a bare value sets, and ``{ set = … }``, ``{ unset = true }``,
``{ prepend = … }``, ``{ append = … }`` name one operation, with an optional
``separator``. Values are literal: there is no template language here, and a
string that looks like one is passed through as written.

The scrub removes the names from the plugin's own process environment before it
composes anything, so the caller ``devenv`` sees is exactly the environment the
runner inherits. It is also why a stale ``MESA_*`` knob in your shell cannot
leak into a test.

The runner is spawned with the scrubbed environment plus the final overlay,
from the run directory as its cwd. The overlay — what the tool changed, not the
whole environment — is recorded in the archive.

Installation
------------

``run`` never installs. A missing asset, stamp or selection fails and names the
``wits gputest install`` command that fixes it.

``wits gputest install [<suite>…]`` derives the project list from the selected
suites (the runner projects included), builds ``release`` on each project's main
branch with the install phase, and verifies the kind's installed layout before
it writes the stamp. It removes the old stamp *before* the build starts, so a
failed install leaves no stamp for a run to mistake for a good tree.

.. code-block:: sh

   wits gputest install                  # every declared suite
   wits gputest install deqp-vk piglit   # only these suites
   wits gputest install -n               # print the build commands and roots

The project's own ``install`` hook is still the installer. A dEQP hook stages
the binary, data, caselists and waivers because VK-GL-CTS has no ``install()``
rule for ``deqp-vk``; the runner hook stages the cargo binaries; Piglit uses
CMake's install rules.

``gputest`` defines **no default install root**. The suite root is the suite
project's ``install_dir`` and the runner root is the runner project's
``install_dir``; both are declarations in the project registry. A path under
``$XDG_RUNTIME_DIR`` is a valid choice, with whatever lifetime that choice
has. The two roots **must not contain each other**: each install stamp hashes
its whole installed tree, so an overlapping root would make one project's
stamp change when the other installs. ``check``, ``install``, ``plan`` and
``run`` refuse an overlapping layout. Siblings such as
``$XDG_RUNTIME_DIR/wits/gputest/{deqp,piglit,runner}`` are disjoint.

The stamp is ``<install_dir>/.gputest.toml``: the canonical tree hash, the
install time, the project key, the branch and the commits. It is part of the
installed tree, not of gputest's state. A tree installed by a bare ``wits build
--install`` has no stamp; install it through ``wits gputest install``.

The verbs
---------

.. code-block:: sh

   wits gputest check                     # config, registry, assets, stamps
   wits gputest list                      # cases, last run, asset state
   wits gputest plan <case>…              # env, argv, identity, baseline
   wits gputest install [<suite>…]        # install test suites; never tests
   wits gputest run <case>… [--if-changed] [profile flags]
   wits gputest history <case> [--limit N]
   wits gputest show <case> <id> [--failures]
   wits gputest prune

``check``
   Validates the config tree, every referenced project, every suite's kind
   config, its assets and the install stamps. It does not compose a driver or
   resolve a build. It exits non-zero when any class of problem is found.

``list``
   One line per case: last run id, exit, and whether the assets and stamps are
   present. It does not compose a driver, so it stays cheap.

``plan``
   Everything a run would do, printed: the composed environment's set/unset,
   the kind's argv, the identity and its components, the baseline, and the
   paths. Use it when "why is this loading that driver?" is the question.

``install``
   The installation act above.

``run``
   The run pipeline. ``--if-changed`` skips a build whose identity was already
   tested and records nothing; without it a run always runs. A rerun of the
   same identity compares against the stored baseline, not against itself.

``history``
   The archives for one case, newest first, read from each archive's first
   member. No CSV is parsed.

``show``
   One archive's ``run.toml``; ``--failures`` adds its ``failures.csv``.

``prune``
   Removes archives older than ``store.archive_days``, stale ``.part`` files
   and empty directories. State is left alone. A ``.part`` is a crash leftover;
   do not run ``prune`` while a run or install is writing one.

``-n``/``--dry-run`` is the plugin's own flag (``wits`` consumes its global
one before dispatching). ``run -n`` prints the plan; ``install -n`` and
``prune -n`` print what they would do. Composition and stamp reads still
happen under ``run -n``, because the plan cannot be printed without them.

Which build
-----------

The driver table names the default build. The ad-hoc flags are the ones
:doc:`build` and :doc:`devenv` already use:

* ``-b``/``--branch``, ``-B``/``--build-type``, ``-T``/``--toolchain``,
  ``-G``/``--generator``, ``-p``/``--preset``, ``--focus``, ``--work-dir``,
  ``--spec``;
* ``--detach`` for a detached checkout;
* ``--build-dir``/``--install-dir`` overrides.

A review checkout is therefore:

.. code-block:: sh

   wits gputest run radv-vk --work-dir ../mesa.review --detach -B release \
       --build-dir ../mesa.review/_build/review-release

Identity
--------

Identity answers one question: *is this the same thing we already tested?* It is
one SHA-256 over seven component hashes:

.. list-table::
   :header-rows: 1
   :widths: 24 76

   * - Component
     - What it covers
   * - ``suite``
     - the installed suite tree, from its install stamp
   * - ``runner``
     - the installed runner tree, from its install stamp
   * - ``driver.icd``
     - the selected ICD manifests and the libraries they name
   * - ``driver.libs``
     - every ``*.so*`` directly under the loader paths, following symlinks
   * - ``driver.config``
     - the EGL vendor files and every ``*.conf`` under ``DRIRC_CONFIGDIR``
   * - ``argv``
     - the kind's argv with the run directory replaced by a marker and
       ``--baseline`` omitted
   * - ``env``
     - the final applied overlay relative to the scrubbed caller

The suite and runner trees are hashed once at install. The driver's content is
hashed at run time. Content, not commits: a failed nightly can leave the
previous build tree in place while the checkout has moved, and content is what
actually loads. Commits are recorded as provenance.

State and baselines
-------------------

State is one small file per case and device:

``$XDG_STATE_HOME/wits/gputest/<case>/<device>.toml`` (with the usual
``$HOME/.local/state`` fallback).

.. code-block:: toml

   hash     = "sha256:…"         # what the last completed run tested
   run      = "20261026T213012Z" # the archive it produced
   baseline = "20261023T…"       # what that run compared against

* No state file — run without a baseline.
* The hash differs — use the previous archive as the baseline, then remember
  the previous run.
* The hash is equal and ``--if-changed`` is given — skip and record nothing.
* The hash is equal without ``--if-changed`` — run anyway, but keep the stored
  baseline, so a rerun returns the same verdict rather than comparing the build
  with itself.
* The baseline archive is gone — run without a baseline and say so. Pruning
  evidence never resurrects a run.
* The run did not complete — a signal, a spawn failure, a missing asset or
  stamp, or a runner that exits 0/1/2 without the expected CSVs is an error,
  not a result. The state is left untouched.

The baseline is the previous archive's ``failures-all.csv``, not its
``failures.csv``: the latter omits expected failures, so using it as a baseline
would make a persistent failure alternate between ``Fail`` and
``ExpectedFail``. The state is not the store's index; a state whose archive has
aged out still decides that the build was tested.

Evidence
--------

One run that reaches the runner produces one archive:

::

   <store>/<case>/<device>/<id>.tar.zst
     run.toml                 # first member
     results.csv              # every result, with timings
     failures.csv             # unexpected results — the new-regression evidence
     failures-all.csv         # unexpected plus expected failures — the next baseline
     expectations.json        # the runner's expectation record

``run.toml`` has ``started``, ``duration``, ``exit``, ``env`` and ``argv`` at
the top, then ``[device]``, ``[driver]``, ``[test]``, ``[runner]`` and
``[content]``. ``env`` holds the overlay's sets; ``unset`` (when non-empty)
names what it removed. The content hashes record exactly what was tested; the
commits record readable provenance.

The archive keeps whatever evidence the runner produced, even on a signal or an
unexpected exit. It is written in the run directory, copied into the store as a
``.part`` file, and renamed over the destination; the rename is inside the
store's filesystem, so a crash never leaves a truncated archive under a run id.

The run directory is ``$XDG_RUNTIME_DIR/gputest/<case>/<id>/`` (falling back to
``/run/user/<uid>/gputest``). It is a runtime directory and disappears with the
session; it is not pruned.

The report and exit status
--------------------------

One line per case on stdout:

::

   radv-vk  20261026T213012Z  regression  new=2 carried=3

The verdict is ``pass``, ``regression``, ``improved``, ``skipped`` or
``error``. New counts are unexpected failures; carried counts are
``ExpectedFail`` results from the baseline.

The ``run`` command exits:

* ``0`` — every case passed or only improved;
* ``1`` — a regression landed;
* ``2`` — a case could not run.

An error outranks a regression. A per-case failure never stops the remaining
cases: the service wants the whole night's picture, not the first failure. A
runner exit of ``2`` (improvements only) is a successful command exit while the
line still says ``improved``.

The nightly
-----------

A service unit only needs:

.. code-block:: sh

   wits gputest run $CASES --if-changed

and, on its own cadence or after a suite change:

.. code-block:: sh

   wits gputest install

The run does not fetch, build or install. Sequence update/build, install and run
in the service that owns them; the memory limit belongs around the run
(``systemd-run --user --scope -p MemoryMax=… -- wits gputest run …``).

When it fails
-------------

.. list-table::
   :header-rows: 1
   :widths: 34 66

   * - Message
     - Cause and fix
   * - ``unknown case …``
     - The name is not in the config. ``check`` catches it before a run; the
       error lists the declared cases.
   * - ``case name … is not a path component``
     - The name is ``.``, ``..``, empty, or holds a character outside
       ``[A-Za-z0-9._-]``. It names the store and state directories.
   * - ``project … is not in the registry``
     - The driver, suite or runner names a project this machine does not
       declare.
   * - ``build directory … does not exist``
     - ``devenv`` cannot compose an environment for a build that was never
       made. Build it first with :doc:`build`.
   * - ``install stamp … is missing``
     - The tree was not installed by ``wits gputest install`` (or that install
       failed before stamping). Run it again.
   * - ``driver exposes no ICD`` / ``select matched N ICDs``
     - The composed environment does not name exactly one Vulkan driver after
       ``VK_LOADER_DRIVERS_SELECT``. Fix the driver's project ``devenv`` or its
       ``select``. An AMDVLK case whose manifest is not installed needs the
       driver's ``wits build --install`` first.
   * - ``suite root … and runner root … overlap``
     - The two installed trees contain each other. Give the suite and runner
       projects disjoint ``install_dir`` values, e.g.
       ``$XDG_RUNTIME_DIR/wits/gputest/{deqp,piglit,runner}``. Each stamp
       hashes its whole tree; an overlap would couple their identities.
   * - ``caselist … does not exist`` / ``… is empty``
     - The install layout and the suite config disagree. Fix the install hook
       or the caselist path; a run never silently tests fewer cases.
   * - ``runner exited with signal …``
     - The runner died; the run is an error, not a regression. The signal is
       recorded as a negative exit in ``run.toml``; the state is left alone.
   * - ``runner exited with code …``
     - A code outside 0/1/2 is an error, not a regression.
   * - ``runner exited N without the expected CSVs``
     - The runner failed before it ran the tests. Exit 1 is also a regression,
       so the verdict is only read when ``results.csv``, ``failures.csv`` and
       ``failures-all.csv`` exist.
   * - ``state file … cannot be read``
     - An unreadable state is an error, not a first run: treating it as empty
       would silently drop the baseline. Delete the file to start over.

Files and paths
---------------

.. list-table::
   :header-rows: 1
   :widths: 30 70

   * - Path
     - Meaning
   * - ``$WITS_GPUTEST_CONFIG`` / ``$XDG_CONFIG_HOME/wits/gputest`` /
       ``$HOME/.wits/gputest``
     - the config tree
   * - ``<store.root>/<case>/<device>/<id>.tar.zst``
     - one archive per run
   * - ``$XDG_STATE_HOME/wits/gputest/<case>/<device>.toml``
     - what the last completed run tested, and its baseline
   * - ``$XDG_RUNTIME_DIR/gputest/<case>/<id>/``
     - the run directory (CSVs, ``baseline.csv``, the archive while written)
   * - ``<project install_dir>/.gputest.toml``
     - the install stamp for a suite or runner tree

What it deliberately does not do
--------------------------------

There is no build, no install during a run, no shell hook, no command template,
no glob expansion, no scheduler, no forge interaction, no multi-machine
dispatch and no machine-output format. The exit status and the archive are the
machine interface; the terminal is for a person. The rationale for each of
these is in :doc:`/reference/gputest-design`.
