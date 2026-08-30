.. _wits-build:

``wits build``
==============

Configure and build a project from the shared registry (:doc:`project`). If
you have not read that chapter, start there — the registry decides everything
``build`` acts on: which repo is the focus, what the build dir is, which
build system, which toolchain.

.. code-block:: sh

   wits build [NAME|PATH]            # the project owning the current directory by default
   wits build hello                  # configure + build, debug, default toolchain
   wits build hello -B release       # build type
   wits build hello -T gcc           # toolchain
   wits build hello -p asan -p lto   # presets

The profile flags
-----------------

These change *resolution* — what ``repos.<name>.workdir``, ``build_dir``, and
``install_dir`` resolve to.

.. list-table::
   :header-rows: 1
   :widths: 26 12 62

   * - Flag
     - Alias
     - Meaning
   * - ``--branch <X>``
     - ``-b``
     - Target branch (the build identity). Default: the focus repo's current
       branch.
   * - ``--build-type <T>``
     - ``-B``
     - Build type (``debug``, ``release``, ``debugoptimized``, …).
   * - ``--toolchain <N>``
     - ``-T``
     - Select a declared toolchain.
   * - ``--generator <G>``
     - ``-G``
     - Build-system generator (e.g. ``Ninja``).
   * - ``--preset <P>``
     - ``-p``
     - Apply a preset; repeatable; accepts ``org/preset``.
   * - ``--focus <repo>``
     -
     - Override which repo is the build focus.
   * - ``--work-dir <DIR>``
     -
     - Use this checkout verbatim as the selected repo's ``repos.<name>.workdir``,
       bypassing the branch strategy's ``worktree_dir``/in-place resolution.
       Everything (``build_dir``/``source_dir``/…) still anchors on it. The
       seam for building a checkout materialised elsewhere — a ``review
       checkout`` worktree.
   * - ``--spec <K=V>``
     -
     - Register a template variable, exposed as ``{{spec.K}}``; repeatable. A
       template that references ``{{spec.K}}`` **requires** it (hard error
       otherwise) — how an out-of-band value (an MR number, a variant tag)
       enters resolution without living in the file.

``--work-dir`` and ``--spec`` are profile axes, so they work on the ``project``
read queries (``build-dir``, …) as well as ``build``.

The build-only flags
--------------------

These control the build *steps*.

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
     - Override the resolved ``build_dir``, ignoring the focus/anchor template
       — e.g. to build a ``review checkout`` in an isolated dir. The symmetric
       partner of ``--install-dir``; verbatim, highest priority.
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
interpreted by the tool — ``-DFOO=BAR`` is handed to cmake verbatim:

.. code-block:: sh

   build hello --extra-config-args -DFOO=BAR --extra-config-args -DBAZ=1
   build hello -Xconfig,-DFOO=BAR         # short form, scope = config|build|install
   build hello -Xbuild,-j8

Modes
-----

``auto`` (the default) configures if needed, then builds. The four explicit
modes are mutually exclusive:

.. list-table::
   :header-rows: 1
   :widths: 20 80

   * - Mode
     - What happens
   * - ``auto`` *(default)*
     - configure if needed, then build
   * - ``config-only``
     - configure, then stop
   * - ``build-only``
     - compile assuming already configured; an unconfigured build dir is an
       error
   * - ``reconfig``
     - delete the build dir and configure fresh
   * - ``uninstall``
     - reverse the last install (meson's ``ninja -C <build> uninstall``,
       cmake's ``install_manifest.txt``; cargo unsupported) — never a
       recursive delete, because an install prefix may be shared

``--install`` adds an install step to a build; it composes with ``auto`` and
the compile modes.

Backends
--------

``build_system`` selects how ``wits`` translates your toolchain and emits the
command steps. Three ship: **cmake**, **meson**, and **cargo**.

The toolchain's canonical fields (``cc``, ``cxx``, ``rustc``, ``ar``, …) are
translated to the backend's native spelling automatically:

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
need and which can conflict with its cached compiler.

Multi-config cmake generators (Ninja Multi-Config, Visual Studio, Xcode) are
handled correctly: ``CMAKE_BUILD_TYPE`` is *not* set at configure, and the
build type is selected at build/install time with ``--config``.

Whether a build dir is already configured is detected per backend:
``CMakeCache.txt`` for cmake, ``meson-private/coredata.dat`` for meson; cargo
has no configure step.

Building a detached review snapshot
-----------------------------------

``review checkout`` materialises an MR at a detached ``HEAD``. From that
worktree, opt in to building the snapshot as-is:

.. code-block:: sh

   wits review checkout 123
   cd ../hello.review
   build --detach

Without ``--detach``, a detached ``HEAD`` remains an error; use ``--branch X``
for a branch-backed build. ``--detach`` and ``--branch`` are mutually
exclusive. Detached mode neither invents a branch identity nor switches a
checkout, so ``branch.raw`` and ``branch.slug`` are absent. A template that
requires either fails hard; use a checkout-keyed template such as
``"{{repos.main.workdir}}/_build/{{build_type}}"``, or pass an explicit
``--build-dir`` / ``--install-dir``. These CLI paths bypass the corresponding
templates.

``--work-dir`` remains an independent location override. It does not imply
detached mode, but may be combined with it when invoking the build from
elsewhere:

.. code-block:: sh

   build hello --detach --work-dir ../hello.review

The build pipeline in one paragraph
-----------------------------------

The configuration is assembled in one strictly single-directional pass, top to
bottom, and no layer is revisited:

toolchain injection (L0) → org config (L0.5) → project config (L1) → presets
(L2) → CLI extra args (L3). The final set of ``environment``, ``definitions``,
and the three ``extra_*_args`` lists is handed to the backend, which turns them
into the exact command lines. A toolchain's compiler identity is an immutable
input — presets cannot clobber it; changing the compiler means moving along
the selection chain (``env (WITS_PROJECT_TOOLCHAIN)`` → ``--toolchain`` → the
project's field, resolved against ``[toolchains]``). The precise rules and the
template language are in
:doc:`/reference/project-reference`.
