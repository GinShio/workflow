.. _installation:

Installation
============

``wits`` is a Cargo workspace: the ``wits`` binary, the shared ``wits-util``
library, and any plugin crates. Two build systems work together, by design.
**Meson** owns the build and install orchestration; **cargo** does the actual
Rust compilation underneath. That split is not an accident. Some dependencies
ship native-code build scripts (notably ``ring``, pulled in by the forge's HTTPS
client) that Meson's native Rust support cannot build without hand-porting their
``build.rs``. So cargo owns compilation, and Meson owns the parts cargo is bad
at: install, symlinks, packaging.

Using meson
-----------

::

   meson setup build              # configure
   meson compile -C build         # build
   meson install -C build         # install

Install under your home directory with ``--prefix``::

   meson setup --prefix ~/.local build

Meson honours ``DESTDIR`` for packaging, so ``DESTDIR=… meson install`` works
the way you expect.

Requirements: meson ≥ 0.61.0, cargo, ninja, and python3 (the cargo→meson bridge
is a short inline Python program; see below). The ``wits`` binary and every
``wits-<sub>`` symlink land in ``<prefix>/bin``.

Build-type mapping
~~~~~~~~~~~~~~~~~~

``meson setup --buildtype=…`` actually controls the Rust build; Meson owns this
one coarse knob, while cargo keeps the fine-grained compile configuration
(``rustflags``, profiles, per-dependency settings):

.. list-table::
   :header-rows: 1

   * - ``--buildtype``
     - cargo profile
     - Build directory
   * - ``release``
     - ``--release``
     - ``release``
   * - ``debugoptimized``
     - ``--release``
     - ``release``
   * - ``minsize``
     - ``--release``
     - ``release``
   * - ``debug`` (or nothing)
     - *none*
     - ``debug``

How the bridge works (the one moving part)
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

Meson's ``custom_target`` requires its command to leave the product at
``@OUTPUT@``, while cargo always writes under ``<target-dir>/<profile>/``.
The bridge — a short inline Python program running inside Meson's own runtime —
builds with cargo and then copies the binary into place. Cargo does its own
up-to-date checking, which is why the target is *always* stale from Meson's
point of view; the build is only ever as expensive as cargo says it is.

For plain development
---------------------

Cargo alone is enough, no Meson required::

   cargo build            # binary at target/debug/wits (the debug profile)
   cargo build --release  # …or at target/release/wits
   cargo test             # unit and integration tests

There is also the plugin crate ``wits-scaffold``, built the same way (binary
beside ``wits`` in the same profile directory). Put the profile directory on
your ``$PATH`` and both the umbrella and the plugin work without any install
step.

What ``meson install`` produces
-------------------------------

  * the ``wits`` binary;
  * one ``wits-<name>`` **applet symlink** per built-in subcommand, pointing at
    ``wits``;
  * the ``wits-scaffold`` plugin binary.

The applet list in ``meson.build`` is explicit on purpose — Meson deliberately
has no source-tree globbing, and the authoritative set of built-ins is the
``Commands`` enum in ``crates/wits/src/main.rs``. The two stay in step, and a
runtime cross-check exists for exactly that day you wonder whether they have:

.. code-block:: sh

   wits __applets          # one built-in name per line

Invocation forms
----------------

A built-in subcommand can be called two ways. The umbrella form and the applet
(direct) form:

.. code-block:: sh

   wits transcrypt status      # umbrella
   wits-transcrypt status      # direct — a symlink to wits

The direct form is a symlink whose name ``wits`` reads from ``argv[0]`` and
splices back in as the subcommand. Same binary, no second process; applet names
come straight from the subcommand list, so a new built-in earns its symlink for
free. Only the ``wits-<sub>`` (dash) spelling is an applet. A bare ``wits.stack``,
a bare ``stack`` symlink, or anything non-dashed is *not* — that spelling was
retired, and the unit tests assert it stays retired.

Anything that is not a built-in is dispatched to a plugin:

.. code-block:: sh

   wits scaffold rest-of-command      # runs `wits-scaffold rest-of-command` from $PATH
   wits-scaffold rest-of-command      # or call the executable directly

Plugin dispatch replaces the process with the plugin (``exec``), so the plugin
owns the terminal and its exit status is yours. The full plugin contract is in
:doc:`plugins`.

Uninstall
---------

Reverse ``meson install`` with:

.. code-block:: sh

   meson uninstall -C build

For a manual cargo layout, remove the ``wits`` and ``wits-<name>`` symlinks
yourself — there is no other state; ``wits`` keeps nothing outside
``$XDG_CONFIG_HOME`` / ``$XDG_STATE_HOME`` and your git config (each command
chapter says exactly which files it reads and writes).

Verifying the install
---------------------

.. code-block:: sh

   wits                        # top-level help, including discovered plugins
   wits --version
   wits worktree --help        # and so on for every subcommand

The top-level help ends with a ``Plugins (wits-* found on PATH)`` section
listing every plugin this install can reach — the whole plugin system made
visible without a registry.

Global flags
------------

Two flags are global to every invocation, inherited by every built-in
subcommand. (A plugin receives only the arguments after its name:
``wits -v gpu …`` keeps ``-v`` for ``wits`` itself, while ``wits gpu -v …``
hands ``-v`` to the plugin as its own flag.)

.. list-table::
   :header-rows: 1

   * - Flag
     - Meaning
   * - ``-v``, ``--verbose``
     - Show the underlying git / build / forge commands as they run — the
       layer below ``wits``.
   * - ``-n``, ``--dry-run``
     - Print the mutating commands instead of running them. Read-only queries
       still run, so control flow stays correct: a dry-run still asks git and
       the forge what the world looks like in order to decide what it *would*
       do, then prints the pushes, MR changes, and file writes rather than
       carrying them out.

``--dry-run`` prints its preview on **stdout** and ordinary logs on **stderr**,
so a plan can be captured cleanly: ``wits worktree prune -n > plan.txt`` is a
clean, grep-able plan, not a log dump.

The one command that predates these flags is ``transcrypt``. It only ever reads
(stdin→stdout), so dry-run has nothing to suppress there; the flags exist
because they are part of the contract future commands inherit from the process
layer.
