.. _reference-index:

References and design notes
===========================

The usage guides tell you how to drive the tools. This volume is the layer
under them: precise contracts, exhaustive tables, and the reasoning that
shaped each tool. Nothing here restates a usage guide; when in doubt,
behaviour-for-users lives in :doc:`/commands/index`, precise contracts here.

.. list-table::
   :header-rows: 1
   :widths: 34 66

   * - Document
     - What it is
   * - :doc:`project-reference`
     - The exhaustive reference for the project system: every configuration
       key, every CLI flag, the template language, and the resolution rules.
   * - :doc:`project-design`
     - The reference design for ``project`` — the agreed shape and the
       reasoning behind each decision.
   * - :doc:`stack-behavior`
     - The authoritative description of *how ``stack`` verbs decide what to
       do*: forks, multi-round edits, deleted branches. The usage guide is
       :doc:`/commands/stack`.
   * - :doc:`stack-design`
     - Why ``stack`` is shaped the way it is: local topology given, remote
       owned.
   * - :doc:`review-json`
     - The JSON contract between ``wits review`` and an editor (or any
       front-end): read payloads via ``--json``, the ``local.json`` write
       contract.
   * - :doc:`review-store`
     - Where review state lives on disk, and how to carry an in-progress
       review to another machine.
   * - :doc:`review-design`
     - Why ``review`` is shaped the way it is: forge-first acquisition,
       snapshots and outdating, the honest forge capability matrix.
