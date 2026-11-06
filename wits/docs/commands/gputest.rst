.. _wits-gputest:

``wits gputest``
================

Run a GPU test suite against a wits build, compare the result with the previous
build, and keep one self-describing archive.

.. code-block:: sh

   wits gputest run radv-vk --if-changed   # the nightly question
   wits gputest run radv-vk                # always run, compare with the last
   wits gputest run radv-vk --group ray-tracing  # only these tests, same baseline
   wits gputest run radv-vk -n             # why is it loading that driver?
   wits gputest list                       # what ran last, and can each case run?
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

The five nouns
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
   * - **layer**
     - A translation layer such as zink, not a Vulkan loader layer: the API a
       suite tests, implemented on the driver a case attaches it to. It names
       the project it comes from, the environment that turns it on, and what
       it loads and reads.
   * - **suite**
     - One runner invocation: a runner shape, the test project whose
       ``install_dir`` is the suite root, the named groups of tests it offers,
       and the runner arguments.
   * - **case**
     - A named ``driver × suite`` and the suite's groups it runs, with an
       optional layer and environment, and the unit of history, identity, and
       baseline.
   * - **run**
     - One execution: an id, a run directory, one archive in the store, and at
       most one state update.

The runner's individual entries are **tests**, not cases; a caselist names
tests, a group names caselists, and the case names the comparison.

Configuration
-------------

The config is a tree of ``*.toml`` files under one root:
``$WITS_GPUTEST_CONFIG``, else ``$XDG_CONFIG_HOME/wits/gputest``, else
``$HOME/.wits/gputest``. Files may sit anywhere under it; a section decides what
a file contributes.

``[store]``, ``[driver.*]``, ``[layer.*]``, ``[suite.*]`` and ``[case.*]``
accumulate across the tree. The same name twice is a load-time error, not an override; an unknown
section or key is a load-time error too. An unattended test run cannot afford a
typo that silently does nothing.

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

   [layer.zink]
   project = "mesa"               # where the layer comes from
   env = { MESA_LOADER_DRIVER_OVERRIDE = "zink" }

   [suite.deqp-vk]
   project = "deqp"               # suite root = its install_dir
   jobs = "cores"                 # physical cores; or a number
   timeout = 300

   [suite.deqp-vk.groups]         # what a case or a narrowed run selects
   all = ["mustpass/vk-default.txt"]
   ray-tracing = [
       "mustpass/vk-default/ray-query.txt",
       "mustpass/vk-default/ray-tracing-pipeline.txt",
   ]
   shader-object = [
       "mustpass/vk-default/pipeline/shader-object-*.txt",  # a pattern
       "mustpass/vk-default/shader-object/",                # a directory
   ]

   [suite.deqp-vk.deqp]           # the runner shape is this table's name
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
   project = "piglit"
   jobs = 4
   timeout = 300

   [suite.piglit.groups]
   quick = ["quick"]              # exactly one profile per group

   [suite.piglit.piglit]
   folder = "lib64/piglit"        # the CMake-installed data directory

   [suite.piglit.env]
   PIGLIT_PLATFORM = "gbm"
   PYTHONDONTWRITEBYTECODE = "1"

   [case.radv-vk]
   driver = "radv"
   suite = "deqp-vk"
   groups = ["all"]

   [case.radv-vk-rt]              # its own history, of the ray tracing lists
   driver = "radv"
   suite = "deqp-vk"
   groups = ["ray-tracing"]

   [case.radv-zink-gl]
   driver = "radv"
   layer = "zink"                 # Piglit's GL through zink, on radv
   suite = "piglit"
   groups = ["quick"]

.. list-table::
   :header-rows: 1
   :widths: 22 78

   * - Section
     - What it says
   * - ``[store]``
     - ``root`` is required and absolute (``~`` is expanded), and it must
       exist: a run creates ``<case>/<device>`` beneath it but never the root
       itself, so a share that is not mounted is an error rather than a copy
       on the local disk under the mount point. ``archive_days`` defaults to
       360 and is what ``prune`` reads. The store is per machine: state and
       baselines are local.
   * - ``[driver.*]``
     - ``project`` is required. ``build_type`` defaults to ``release``;
       ``branch`` defaults to the resolved focus's identity-repo
       ``main_branch`` (the focus repo when it has its own git, else its
       anchor). ``select`` is set as ``VK_LOADER_DRIVERS_SELECT``. ``env`` is a
       ``devenv`` declaration table applied after ``devenv``. ``libs`` lists
       the libraries the driver loads besides its ICD's own, and ``config``
       the settings it reads, such as ``~/.drirc`` or a file its ``env``
       points it at. Each entry is an absolute path (``~`` is expanded) to a
       file, or to a directory that stands for every file directly inside it,
       and must exist when the case runs. A ``libs`` entry without a ``/`` is
       instead a library the ICD links, named without its version
       (``libLLVM.so`` for ``libLLVM.so.23.1``) and found the way the dynamic
       loader finds it in the case's environment; naming nothing the ICD
       links is an error. Their content is the identity's ``driver.libs`` and
       ``driver.config``; gputest adds nothing to either on its own.
   * - ``[layer.*]``
     - ``project`` is required: where the layer comes from. ``env`` turns it
       on and is applied after the driver's; ``libs`` and ``config`` take a
       driver's entries and are the identity's ``layer.libs`` and
       ``layer.config``. On a driver of the same project the layer runs from
       the driver's build, whichever build the run selects. On a driver of
       another project it brings its own: its project's release build on its
       main branch, composed beneath the driver's, so where both set a
       variable the driver's value wins and the ICD stays the driver's.
   * - ``[suite.*]``
     - ``project`` is required; ``runner`` defaults to ``runner``. ``jobs``
       defaults to physical cores; ``timeout`` (seconds per test) to 60.
       ``env`` is applied after the driver. A suite declares at least one
       group, and exactly one of the two shape tables below names its runner
       shape.
   * - ``[suite.<name>.groups]``
     - Each key names a group, ``[A-Za-z0-9._-]+``, and its value lists what
       the group runs; no group is empty. A name with a ``.`` is quoted
       (``"image.layout" = […]``), since a bare dotted key declares a nested
       table instead. For ``deqp`` an entry is a caselist
       file, one ``--caselist`` each, or names several: one that ends in
       ``/`` stands for every ``.txt`` list directly inside that directory,
       and one holding ``*`` or ``?`` for every ``.txt`` list it matches, a
       ``*`` standing for any run of characters and a ``?`` for one, neither
       crossing a ``/``. A caselist may also be an index, as VK-GL-CTS's
       mustpass ``vk-default.txt`` is: deqp-runner reads each line that ends
       in ``.txt`` as another caselist beside the index. For ``piglit`` a group
       names exactly one profile, never a pattern.
   * - ``[suite.<name>.deqp]``
     - ``exe`` is required; ``waiver``, ``args`` and ``tests_per_group`` are
       optional.
   * - ``[suite.<name>.piglit]``
     - ``folder`` is required: the directory holding ``tests/``, ``bin/`` and
       ``lib/``. Piglit takes exactly one profile per invocation.
   * - ``[case.*]``
     - ``driver``, ``suite`` and ``groups`` are required. ``groups`` names at
       least one of the suite's groups, each once, and exactly one for a
       ``piglit`` suite. ``layer`` attaches a declared layer to the driver;
       ``env`` is applied last. The case name is a path component and must
       match ``[A-Za-z0-9._-]+``.

Every suite path (``exe``, ``waiver``, each group entry, ``folder``) is
relative to the suite root and must not escape it: absolute paths and
components that climb out with ``..`` are load-time errors, because the install
stamp hashes that tree and a file outside it would be tested without being part
of the identity. A shape's keys live only in its own table; a ``deqp`` key in a
``piglit`` table is an unknown key. A suite's subtables belong to the file that
declares the suite: in another file, ``[suite.<name>.groups]`` starts a suite of
its own, which fails to load for want of a ``project``.

There is no ``enabled`` flag and no schedule. Which cases run nightly is a
service declaration; the config declares what a case *is*, not when it runs.

Runner shapes
-------------

A shape is one runner invocation, not one API. Both run a binary from the
``deqp-runner`` project, and gputest reads the same evidence from both.

* ``deqp`` — dEQP Vulkan through ``deqp-runner``. The installed tree needs the
  dEQP executable, ``vulkan/`` and the waiver, and the caselists a run
  selects. Its driver guard requires exactly one selected Vulkan ICD.
* ``piglit`` — Piglit GL through ``piglit-runner``. The installed tree is the
  CMake install output (``<folder>/{tests,bin,lib}``) and the profile a run
  selects. Its driver guard is empty; a case reaches a Vulkan driver through a
  layer such as zink.

Adding a shape is a code change, not a config value: a ``Kind`` variant
carrying its table, and its arms in ``runner_bin``, ``check_group_count``,
``select``, ``argv``, ``check_assets`` and ``check_driver``. The pipeline never
branches on the shape itself.

Groups
------

A group is a named set of a suite's tests. A case runs the groups it names,
and ``run --group`` narrows a run to others; both select from one suite,
because a selection is what one runner invocation runs. A ``piglit`` selection
is one group and its one profile.

* **A set.** A selection runs every test its groups reach, each once:
  deqp-runner neither deduplicates its test list nor notices a cycle, so a
  test listed twice would run twice and its second result would be recorded
  as a failure. An entry whose caselists another selected entry reaches too is
  dropped and the larger kept, so ``all`` with any group inside it runs as
  ``all``, and a list that two groups name runs once. The runner reads each
  entry whole, so an index that names a caselist twice, or names itself, is
  an error, and so are two indexes that share caselists without one holding
  the other. Overlap is judged by caselist, not by test.
* **One order.** The groups are taken in name order and a repeated name counts
  once, so one set of groups always reaches the runner in one order. The order
  is not cosmetic: deqp-runner shuffles its tests with a fixed seed, so it
  decides which tests share a dEQP process, and the identity hashes it as part
  of the argv. Reordering a case's ``groups``, or adding a group whose
  caselists the selection already reaches, changes neither what runs nor its
  identity.
* **Read as the runner reads them.** To know what a selection reaches, gputest
  reads every caselist in it the way deqp-runner does: a line that ends in
  ``.txt`` names another caselist beside the one holding it, and any other line
  is a test. Each caselist must be a non-empty file inside the suite root.
  gputest reads caselists only as plain text, so a ``.txt.zst`` line, or a
  ``.zst`` file beside a caselist (which deqp-runner would open instead), is
  an error.
* **Patterns follow the tree.** A directory or pattern entry is expanded when
  a run selects its group, into the lists it matches in byte order, each of
  which counts like a list named on its own. It follows the installed tree: a
  list that upstream adds, renames or removes there joins or leaves the group
  without an error, so a pattern suits a directory that is one category as a
  whole. One that matches nothing is an error.
* **Checked when selected.** ``install`` checks the shape's layout and not what
  the groups name, so a broken group fails only the runs that select it and
  never one that does not. ``run`` and ``run -n`` check the selection before
  the driver is composed; ``list`` checks each case's own.

``--group`` narrows every named case to the given groups for one run. The run
is compare-only: it compares against the case's state, is archived with the
groups it ran, and leaves the state alone, so the case's next run still
compares with a run of the case as declared. What the groups reach must be
inside what the case runs, because the baseline is the case's last run, which
never judged any other test: each of their known failures would count as new.
``--group`` and ``--if-changed`` exclude each other: the state records only runs
of the case as declared, so it cannot say whether a ``--group`` selection was
already tested.

Groups that deserve a history of their own belong in a case of their own:
``radv-vk-rt`` above keeps its own state, store directory and baseline.

Environment
-----------

A run's environment is built in one direction and no step is revisited:

#. The caller's environment, scrubbed of graphics and loader state: ``VK_*``,
   ``MESA_*``, ``LIBGL_*``, ``EGL_*``, ``__EGL_*``, ``GLX_*``, ``__GLX_*``,
   ``RADV_*``, ``R600_*``, ``RADEON_*``, ``ZINK_*``, ``NIR_*``, ``RUSTICL_*``,
   ``OCL_ICD_*``, ``GBM_*``, ``AMD_*``, ``AMDVLK_*``, ``LIBVA_*``,
   ``VDPAU_*``, plus ``LD_LIBRARY_PATH``, ``LD_PRELOAD``, ``LD_AUDIT``,
   ``GALLIUM_DRIVER``, ``DRIRC_CONFIGDIR``, ``DRI_PRIME``,
   ``DRI_DEVICE_NAME`` and ``WITS_DEVENV``. ``PATH`` is not scrubbed.
#. The ``devenv`` of the layer's own build, when the layer comes from another
   project than the driver's.
#. The ``devenv`` of the driver's build: the build system's base (Meson's own
   ``meson devenv``, run in the environment so far) plus the project's
   declared ``devenv`` operations.
#. ``driver.select``, then the driver's ``env`` table.
#. The layer's ``env`` table.
#. The suite's ``env`` table.
#. The case's ``env`` table.

The four test-only tables use the same vocabulary as the project registry's
``devenv``: a bare value sets, and ``{ set = … }``, ``{ unset = true }``,
``{ prepend = … }``, ``{ append = … }`` name one operation, with an optional
``separator``. Values are literal: there is no template language here, and a
string that looks like one is passed through as written.

The scrub removes the names from the plugin's own process environment before
the registry loads, so the caller ``devenv`` sees is exactly the environment
the runner inherits. It is also why a stale ``MESA_*`` knob in your shell cannot
leak into a test.

The runner is spawned with the scrubbed environment plus the final overlay,
from the run directory as its cwd. The overlay — what the tool changed, not the
whole environment — is recorded in the archive.

Installation
------------

``run`` never installs. A missing asset or stamp fails before the driver is
composed and names the ``wits gputest install`` command that fixes it; a
caselist or profile that a selection names and the installed tree lacks fails
there too, and names that file.

``wits gputest install [<suite>…]`` derives the project list from the selected
suites (the runner projects included), builds ``release`` on each project's main
branch with the install phase, and verifies the shape's installed layout before
it writes the stamp; what the groups name is checked when a run selects it. It
removes the old stamp *before* the build starts, so a
failed install leaves no stamp for a run to mistake for a good tree. Each
project installs on its own: one that fails is reported, the others still
install, and the command exits non-zero at the end.

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
has. The roots **must not contain each other**: each install stamp hashes its
whole installed tree, so a root inside another would make that project's stamp
change when the other installs. ``install`` checks every pair of roots before
anything builds. Siblings such as ``$XDG_RUNTIME_DIR/wits/gputest/{deqp,piglit,runner}``
are disjoint.

The stamp is ``<install_dir>/.gputest.toml``: the canonical tree hash, the
install time, and a ``[source]`` table with the project key, the branch and the
commits. It is part of the installed tree, not of gputest's state. A tree
installed by a bare ``wits build --install`` has no stamp; install it through
``wits gputest install``.

The verbs
---------

.. code-block:: sh

   wits gputest list                      # newest archive per case; can it run?
   wits gputest install [<suite>…]        # install test suites; never tests
   wits gputest run <case>… [--if-changed | --group <group>…] [profile flags]
   wits gputest run <case>… -n            # the plan: env, argv, identity, baseline
   wits gputest history <case> [--limit N]
   wits gputest show <case> <id> [--failures]
   wits gputest prune

``list``
   One line per case: its newest archive on this device with the groups it
   ran, the verdict and counts, and ``[not ready: …]`` when the case cannot run
   now — its driver's project is not in the registry, its suite or runner is
   not installed, or its groups do not select from the installed tree as
   `Groups`_ requires. It exits
   1 when any case cannot run, so it is the check before a long run. It reads
   stamps, layouts, the caselists each case selects and ``run.toml``, and never
   composes a driver.

``install``
   The installation act above.

``run``
   The run pipeline. ``--if-changed`` skips a case whose identity was already
   tested and records nothing; without it a run always runs. A rerun of the
   same identity compares against the stored baseline, not against itself.
   ``--group`` narrows each case to those groups for one compare-only run (see
   `Groups`_). ``-n`` prints everything a run would do instead: the groups, the
   composed environment's set and unset, the argv, the identity and its
   components, the baseline and the state. Composition, stamp and caselist
   reads still happen, because the plan cannot be printed without them.

``history``
   The archives for one case, newest first: id, device, driver commit, the
   groups it ran, verdict and counts, each read from the archive's
   ``run.toml``.

``show``
   One archive's ``run.toml`` as written; ``--failures`` adds the runner's
   ``failures.csv``.

``prune``
   Removes archives older than ``store.archive_days``, stale ``.part`` files
   and the directories that leaves empty. State is left alone. A ``.part`` is
   the leftover of a run that crashed while writing its archive; do not run
   ``prune`` while a run is writing one.

``-n``/``--dry-run`` and ``-v``/``--verbose`` are the plugin's own flags;
``wits -n gputest …`` forwards them, so it means the same as
``wits gputest -n …``. ``install -n`` and ``prune -n`` print what they would
do.

Which build
-----------

The driver table names the case's build. The ad-hoc flags are the ones
:doc:`build` and :doc:`devenv` already use:

* ``-b``/``--branch``, ``-B``/``--build-type``, ``-T``/``--toolchain``,
  ``-G``/``--generator``, ``-p``/``--preset``, ``--focus``, ``--work-dir``,
  ``--spec``;
* ``--detach`` for a detached checkout;
* ``--build-dir``/``--install-dir`` overrides.

A run given any of them is **compare-only**: it compares against the case's
state and is archived like any run, but it does not move the state, so the
declared build's next run still compares with the declared build. The flags
select the driver's build only; a layer from another project keeps its own.
A review checkout is therefore:

.. code-block:: sh

   wits gputest run radv-vk --work-dir ../mesa.review --detach -B release \
       --build-dir ../mesa.review/_build/review-release

Profile flags combine with ``--group``, so a review checkout can run only the
tests its change touches, against the same baseline:

.. code-block:: sh

   wits gputest run radv-vk --work-dir ../mesa.review --detach -B release \
       --build-dir ../mesa.review/_build/review-release --group ray-tracing

Identity
--------

Identity answers one question: *is this the same thing we already tested?* It is
one SHA-256 over nine component hashes, and the archive records all ten:

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
     - the files the driver's ``libs`` lists, following symlinks, and the
       libraries its bare names find among those the ICD links; a listed
       directory contributes every file directly inside it, skipping one that
       resolves to nothing
   * - ``driver.config``
     - the files the driver's ``config`` lists, read the same way
   * - ``layer.libs``
     - what the case's layer's ``libs`` names, read like ``driver.libs``; the
       empty set without a layer
   * - ``layer.config``
     - the files the case's layer's ``config`` lists, read the same way
   * - ``argv``
     - the runner argv, the selection's caselists or profile in their order,
       with the run directory replaced by a marker and ``--baseline`` omitted
   * - ``env``
     - what the final overlay changed relative to the scrubbed caller; where a
       value extends the caller's own (Meson prepends its build directories to
       ``PATH``), only the added part

The suite and runner trees are hashed once at install. The driver's content is
hashed at run time. Content, not commits: a failed nightly can leave the
previous build tree in place while the checkout has moved, and content is what
actually loads. Commits are recorded as provenance.

``VK_LOADER_DRIVERS_SELECT`` follows the loader's own rule: comma-separated
tokens, each compared case-insensitively with the manifest's file name as a
prefix (``x*``), a suffix (``*x``), a substring (``*x*``) or the whole name.
Any other ``*``, ``?`` and ``[`` are literal, so a selector the loader would
not understand selects nothing here either.

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
* The hash differs — use the last run's archive as the baseline, then remember
  this run.
* The hash is equal and ``--if-changed`` is given — skip and record nothing.
* The hash is equal without ``--if-changed`` — run anyway, but keep the stored
  baseline, so a rerun returns the same verdict rather than comparing the build
  with itself.
* The baseline archive is gone — run without a baseline and say so. Pruning
  evidence never resurrects a run.
* The run did not complete — a signal, a spawn failure, a missing asset or
  stamp, or a runner that exits without ``results.csv`` is an error, not a
  result. The state is left untouched.
* The run is compare-only — the state is left untouched.

The baseline is the previous archive's ``failing.csv``: every test that failed
in that run, with the status it actually had. gputest writes it from
``results.csv`` and the baseline that run used. The runner's own
``failures-all.csv`` cannot serve: it records ``ExpectedFail`` and
``UnexpectedImprovement(Fail)`` as statuses, and the runner matches a
``Timeout`` only against ``Timeout`` and a ``Fail`` only against ``Fail`` or
``ExpectedFail``, so a persistent timeout would alternate between expected and
new, and a crash that became a failure would come back as new. The state is not
the store's index; a state whose archive has aged out still decides that the
build was tested.

Evidence
--------

One run that reaches the runner produces one archive:

::

   <store>/<case>/<device>/<id>.tar.zst
     run.toml                 # first member
     results.csv              # every result, with timings
     failures.csv             # unexpected results, as the runner wrote them
     failures-all.csv         # the runner's failures plus expected ones (deqp-runner ≥ 0.24)
     expectations.json        # the runner's expectation record
     failing.csv              # every failing test by its own status — the next baseline

``run.toml`` has ``started`` (the run id), ``duration``, ``exit``, ``verdict``,
``detail`` (why an error is one), ``baseline`` (the archive compared against),
``compare_only``, ``groups`` (what the run selected), ``argv`` and ``unset`` at
the top, then ``[counts]``,
``[env]``, ``[device]``, ``[driver]`` (including the ``build_dir`` whose
environment the run used), ``[layer]`` (its name and project, and the build it
brought when it comes from another project), ``[suite]``, ``[runner]`` and
``[content]`` (the identity and its nine components).

The archive keeps whatever evidence the runner produced, even on a signal or an
unexpected exit. It is written into the store as a synced ``.part`` file and
renamed over the destination, so a crash never leaves a truncated archive under
a run id.

The run directory is ``$XDG_RUNTIME_DIR/gputest/<case>/<id>/``. It is the
runner's scratch, and it is removed once its archive is in the store; when the
archive cannot be written, it is kept and the error names it. A run id is the
UTC second the run started; a second run of one case within the same second
waits for the next one.

The report and exit status
--------------------------

One line per case on stdout:

::

   radv-vk  20261026T213012Z  regression  new=2 carried=3 improved=0

The verdict is ``pass``, ``regression``, ``improved``, ``skipped`` or
``error``, counted from ``results.csv``: ``new`` are failures the baseline did
not have (``Fail``, ``Crash``, ``Timeout``, ``Missing``), ``carried`` are
failures it had (``ExpectedFail``), and ``improved`` are baseline failures that
no longer fail the same way (``UnexpectedImprovement``). The runner's exit
status must agree — 0 exactly when nothing is new or improved — or the run is an
error; that makes every deqp-runner version read the same way. A compare-only
run ends its line with ``(compare-only)``.

The ``run`` command exits:

* ``0`` — every case passed, improved, or was skipped;
* ``1`` — a regression landed;
* ``2`` — a case could not run.

An error outranks a regression. A per-case failure never stops the remaining
cases: the service wants the whole night's picture.

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
     - The name is not in the config; the error lists the declared cases.
   * - ``case name … is not a path component``
     - The name is ``.``, ``..``, empty, or holds a character outside
       ``[A-Za-z0-9._-]``. It names the store and state directories.
   * - ``project … is not in the registry``
     - The driver, layer, suite or runner names a project this machine does
       not declare.
   * - ``suite … is not installed; run `wits gputest install …```
     - A stamp is missing (the tree was not installed by ``wits gputest
       install``, or that install failed), or the installed layout lacks
       something the suite needs. Run the named command.
   * - ``build directory … does not exist``
     - ``devenv`` cannot compose an environment for a build that was never
       made, the driver's or the one a layer from another project brings.
       Build it first with :doc:`build`.
   * - ``driver exposes no ICD`` / ``… matched N of the ICDs``
     - The composed environment does not name exactly one Vulkan driver after
       ``VK_LOADER_DRIVERS_SELECT``. Fix the driver's project ``devenv`` or its
       ``select``. A driver whose ``devenv`` names the manifest its install
       step writes needs ``wits build --install`` first.
   * - ``the install roots of … overlap``
     - Two installed trees contain each other. Give every suite and runner
       project its own ``install_dir``, e.g.
       ``$XDG_RUNTIME_DIR/wits/gputest/{deqp,piglit,runner}``.
   * - ``suite … has no group …``
     - A case or ``--group`` names a group the suite does not declare; the
       error lists the suite's groups.
   * - ``piglit takes one profile per invocation``
     - A piglit group names several profiles or a pattern, or a piglit case or
       run selects several groups.
   * - ``group …: … matches no caselist``
     - A directory or pattern entry matches no ``.txt`` list in the installed
       tree. Fix the entry, or the install that should have put the lists
       there.
   * - ``caselist … is missing`` / ``… is not a non-empty file``
     - The install layout and a selected group disagree. Fix the install hook
       or the group's entry; a run never silently tests fewer cases.
   * - ``caselist … is reached twice through one entry``
     - An index names a caselist twice, or names itself. deqp-runner reads the
       index whole and would run those tests twice; fix the index.
   * - ``… but neither entry holds the other``
     - Two indexes share caselists and each also reaches some the other does
       not, so neither can be dropped. Select one of them, or name the lists
       instead.
   * - ``group … selects …, which case … does not run``
     - ``--group`` reaches tests the case's own groups do not, which its
       baseline never judged. Narrow a case that runs them, or declare a case
       for them.
   * - ``caselist … names …, which is outside the suite root``
     - An index line is absolute or climbs out with ``..``; the install stamp
       hashes only the suite root.
   * - ``… compressed caselist …`` / ``….zst exists``
     - gputest reads caselists only as plain text; install them uncompressed.
   * - ``runner was killed by signal …``
     - The runner died; the run is an error, not a regression. The signal is
       recorded as a negative exit in ``run.toml``; the state is left alone.
   * - ``runner exited with code …``
     - A code outside 0/1/2 is an error, not a regression.
   * - ``runner exited N without writing results.csv``
     - The runner failed before it ran the tests.
   * - ``runner exited N, but results.csv holds …``
     - The exit status and the results disagree; the run is an error rather
       than a guess.
   * - ``state file … cannot be read``
     - An unreadable state is an error, not a first run: treating it as empty
       would silently drop the baseline. Delete the file to start over.
   * - ``store root … does not exist``
     - The store's filesystem is not there — typically a network share that
       is not mounted. Mount it, or create the directory, and run again.
   * - ``XDG_RUNTIME_DIR is not set``
     - The run directories live there; run under a login session or the user
       service manager.

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
     - the run directory while a run is going (CSVs, ``baseline.csv``)
   * - ``<project install_dir>/.gputest.toml``
     - the install stamp for a suite or runner tree

What it deliberately does not do
--------------------------------

There is no build, no install during a run, no shell hook, no command template,
no glob expansion beyond a deqp group's entries, no scheduler, no forge
interaction, no multi-machine dispatch and no machine-output format. The exit status and the archive are the
machine interface; the terminal is for a person.
