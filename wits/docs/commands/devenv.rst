.. _wits-devenv:

``wits devenv``
===============

Run a program — or a shell — in the runtime environment of one build of a
project. The registry (:doc:`project`) says *which* build; the project declares
what a program needs in order to use it; the build system adds what it already
maintains itself.

.. code-block:: sh

   wits devenv mesa -- vulkaninfo --summary     # one program, against mesa's build
   wits devenv mesa -b feature-x                # a $SHELL in that branch's build
   wits devenv mesa --dump=fish | source        # load it into the current fish

It is the answer to "run this against *that* build": a branch's tree, a review
snapshot, a debug build next to a release one — without re-deriving loader
manifests and library paths by hand each time.

Which build
-----------

``devenv`` takes the same build-identity flags as :doc:`build`, so a program
runs against exactly the tree ``build`` produces for the same flags:

* the profile flags — ``-b``, ``-B``, ``-T``, ``-G``, ``-p``, ``--focus``,
  ``--work-dir``, ``--spec``;
* ``--detach``, for a detached checkout such as a ``review checkout`` worktree;
* the ``--build-dir`` / ``--install-dir`` overrides.

The positional is the same name or path, and with none the project owning the
current directory is meant.

It resolves and **never switches**. An in-place ``--branch`` selects that
branch's build directory and leaves the checkout where it is: a switch would
have to hold for as long as the program runs, a shell included. What it does
require is a build directory that exists — and, for meson, one that is
configured.

How the environment is composed
-------------------------------

Three layers, in order:

#. **The base.** For a meson build, Meson's own developer environment for that
   build directory — what ``meson devenv`` gives, which a project fills through
   ``meson.add_devenv()``. Mesa's build-tree ICD manifests, its ``LD_LIBRARY_PATH``
   and its ``DRIRC_CONFIGDIR`` all come from there, so nothing about them is
   declared or re-derived here. For cmake and cargo, which keep no developer
   environment of their own, the base is yours.
#. **The declared operations**, from the ``devenv`` tables of the registry,
   applied on top of the base.
#. ``WITS_DEVENV``, set to the project — ``org/name``, or the bare name of a
   project in no org — so a prompt or a script can tell it is inside one.

The base is read back as data rather than wrapped around the program:
``meson devenv`` runs a hidden ``wits __devenv-capture``, and the environment
that process inherited comes back exactly. So a declared ``prepend`` lands on
the value Meson set — not on a ``$VAR`` placeholder, which is all Meson's own
``--dump`` can offer — and the result can be printed in any shell's syntax.

The build-time ``environment`` — and the toolchain's ``CC``, ``CFLAGS``, … — is
never part of it. Those configure the build, and have no business in the program
under test.

Declaring a runtime environment
-------------------------------

``devenv`` is a table beside ``environment``, at the same levels:
``[org.devenv]``, ``[project.devenv]``, and the ``devenv`` of any preset.

.. code-block:: toml

   # amdvlk.toml — the manifest the install step writes into the prefix
   [project.devenv]
   VK_DRIVER_FILES = "{{install_dir}}/vulkan/icd.d/amd_icd64.json"

   # a build tree's own tools first on PATH
   [project.devenv]
   PATH = { prepend = "{{build_dir}}/bin" }

   # mesa.toml — nothing to declare at all; a preset can still narrow what loads
   [project.presets.radv-only.devenv]
   VK_LOADER_DRIVERS_SELECT = "*radeon*"

A bare value — a string, a number, a boolean, or a list of them — **sets** the
variable. A table names one operation, in the vocabulary of Meson's
``environment()``:

.. list-table::
   :header-rows: 1
   :widths: 30 70

   * - Entry
     - Effect
   * - ``{ set = V }``
     - Replace the value (the same as a bare ``V``).
   * - ``{ prepend = V }``
     - Put ``V`` in front of the value, joined by the separator.
   * - ``{ append = V }``
     - Put ``V`` after the value, joined by the separator.
   * - ``separator = S``
     - Joins a list ``V``, and the existing value for prepend/append.
       Default ``:``.

A prepend or append onto a variable that is not set yields ``V`` alone; onto
one that is set — even to the empty string — the two are joined. That is
Meson's own rule, and the base is Meson's environment.

Values are templates, resolved against the same context the build's are:
``{{build_dir}}``, ``{{install_dir}}``, ``{{repos.<name>.workdir}}``,
``{{spec.*}}``, ``{{env.*}}``. A ``devenv`` entry never enters ``env.*``
itself, so ``PATH = "/opt/bin:{{env.PATH}}"`` reads the ``PATH`` already there
— yours, unless the build's ``environment`` declares one — where the same line
in ``environment`` would be a cycle. A template that does not resolve fails the
plan as any template does, ``build``'s included, and ``project check`` reports
it.

Operations accumulate
~~~~~~~~~~~~~~~~~~~~~

Unlike ``environment``, where the nearest level's value replaces the others,
``devenv`` operations **accumulate**, as Meson's do, in layer order: the org's,
the project's, then each applied preset's — its ``extends`` first, and a
same-named preset's org, project, and repo levels in that order. Two rules keep
the result predictable:

* A ``set`` discards the operations before it on the same variable, since it
  overwrites whatever they built.
* A prepend or append repeating one still in force is not added again, so a
  preset reached twice through ``extends`` cannot list one ICD manifest twice —
  and load its driver twice.

Printing it: ``--dump[=FORMAT]``
--------------------------------

``--dump`` prints what the environment changes relative to yours, instead of
running anything: every variable it sets or changes, with its real value, and
every one it removes.

.. list-table::
   :header-rows: 1
   :widths: 18 82

   * - Format
     - Output
   * - ``sh`` *(default)*
     - ``export NAME='value'`` / ``unset NAME`` — for ``eval`` in sh, bash, zsh.
   * - ``fish``
     - ``set -gx NAME 'value'`` / ``set -e NAME`` — fish splits a ``*PATH``
       value into its list by itself, and keeps any other whole.
   * - ``json``
     - ``{"set": {…}, "unset": […]}`` — for an editor.

The format must be attached with ``=`` (``--dump=fish``), as with
``review diff --patch``, so that a project name after the flag is never read as
a format.

Running it
----------

``wits devenv … -- <cmd>`` replaces itself with the program, so the program owns
the terminal, receives its signals, and its exit status is the one you see.
Without a command it starts ``$SHELL`` (``/bin/sh`` when that is unset). Either
runs in your working directory, so a relative path in the command means what you
typed.

``-n`` still reads the base — the environment has to be known to be described —
and prints the command it would run instead of running it.

``project info`` shows the same thing under ``devenv:`` — the base a program
starts from, and every declared operation in the order it applies — without
running the build system.

When it fails
-------------

.. list-table::
   :header-rows: 1
   :widths: 44 56

   * - Symptom
     - Cause and fix
   * - ``declares no build_dir``
     - The project has nothing built to run against.
   * - ``build directory … does not exist``
     - Build it first: ``wits build`` with the same flags.
   * - ``build directory … is not configured``
     - A meson directory without ``meson setup`` behind it; ``wits build``
       configures it.
   * - ``has no branch checked out; pass --detach …``
     - The checkout is at a detached ``HEAD``; ``--detach`` uses it as-is,
       ``--branch`` names a branch.
   * - ``meson devenv … failed``
     - Meson's own message follows (a build directory from another Meson
       version, say).
   * - ``devenv variable … is not valid UTF-8``
     - A variable the devenv changes cannot be spelled on a command line or in
       a dump. Variables it leaves alone are inherited untouched, whatever they
       hold.

Variables already in your environment carry through, because the base starts
from it. Mesa's devenv *appends* to ``VK_DRIVER_FILES``, so a devenv started
inside another one loads both builds' drivers.
