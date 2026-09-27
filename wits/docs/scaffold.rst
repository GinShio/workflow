.. _wits-scaffold:

``wits scaffold``
=================

``wits scaffold`` copies facts from the Vulkan and SPIR-V specifications into
the repeated text a source tree expects. When a new extension lands, the same
tokens have to appear in the registry, the grammar tables, the headers, the
dispatch tables, and the docs — scattered enough to be mechanical, mechanical
enough to be wrong every time it is done by hand. This plugin does that
insertion for you, as a reviewed patch.

It is an external plugin, so both invocation forms are equivalent:

.. code-block:: sh

   wits scaffold ...
   wits-scaffold ...

The command has three stages:

.. code-block:: text

   specification ──extract──> descriptor ──generate──> patch or files
                                         └──doctor───> catalogue diagnostics

* A **descriptor** contains extracted facts plus opaque configured metadata
  (``build``). It is TOML, intended to be reviewed and edited.
* A **catalogue** describes one target tree: its root, the specification plane
  it consumes, and the rules that place rendered text.
* A **rule** renders one body and locates its insertion point with an anchor.

The division is the point: specification readers are Rust code because their
input formats are fixed (Khronos-defined, closed, few). Target layout is
configuration because paths, surrounding text, and insertion patterns differ
between trees and change independently of this tool.

Configuration
-------------

The configuration root is selected in this order:

1. ``--config DIR``
2. ``$WITS_SCAFFOLD_CONFIG``
3. ``$XDG_CONFIG_HOME/wits/scaffold``
4. ``~/.config/wits/scaffold``

Its layout::

   env.toml
   kinds.toml
   target/*.toml

Specification sources
~~~~~~~~~~~~~~~~~~~~~

``env.toml`` names the published files read by ``extract``:

.. code-block:: toml

   [source]
   spv_grammar = "/data/specs/spirv.core.grammar.json"
   spv_spec = "/data/specs/extensions"
   vk_registry = "/data/specs/vk.xml"

Each value has a command-line override with the same name:

.. code-block:: sh

   --spv-grammar FILE
   --spv-spec DIR
   --vk-registry FILE

The paths identify files, not projects. This permits selecting an older copy
or moving the containing checkout without changing any catalogue.

Operand kinds
~~~~~~~~~~~~~

``kinds.toml`` selects the SPIR-V operand kinds to extract. ``aliases`` are
lower-cased spellings accepted in prose table headers.

.. code-block:: toml

   [[kind]]
   name = "Capability"
   aliases = ["capability", "capabilities"]

   [kind.meta]
   spelling = "Cap"
   qualified = false

``meta`` is an arbitrary TOML table. The extractor copies it into the matching
descriptor group, and templates may read it as ``kind.meta.*``. The scaffold
engine does not assign meaning to any metadata key.

An operand kind omitted from this file is omitted from extraction.

Extracting a descriptor
-----------------------

.. code-block:: sh

   wits scaffold extract SPV_TEST_widget VK_TEST_widget -o widget.toml

A ``SPV_`` name fills the SPIR-V plane and a ``VK_`` name fills the Vulkan
plane. The planes are independent: pass either one or both.

For SPIR-V, extraction first consults the machine-readable grammar. If it has
no matching entry, the prose specification is read instead. Use
``--spec-file FILE`` to select one prose document directly and skip the grammar
lookup.

The prose reader is heuristic. Its notes are written both to stderr and to the
descriptor so an uncertain result cannot silently become input to generation.
Generation never reads a specification again.

Descriptor shape
~~~~~~~~~~~~~~~~

.. code-block:: toml

   [spv]
   name = "SPV_TEST_widget"
   feature = "TEST_WIDGET"
   snake = "spv_test_widget"

   [[spv.types]]
   name = "OpTypeWidgetTEST"
   aliases = ["OpTypeWidgetAliasTEST"]
   value = 5450
   class = "Type-Declaration"
   capabilities = ["WidgetTEST"]

   [[spv.types.encoding]]
   has_result_type = false
   has_result_id = true
   min_word_count = 2
   variable_word_count = false
   literal_operands = []
   literal_indices_known = true

   [[spv.operations]]
   name = "OpWidgetTEST"
   value = 5451
   class = "Arithmetic"

   [[spv.operations.operands]]
   kind = "IdResultType"

   [[spv.operations.operands]]
   kind = "IdResult"

   [spv.operations.encoding]
   has_result_type = true
   has_result_id = true
   min_word_count = 3
   variable_word_count = false
   literal_operands = []
   literal_indices_known = true

Type declarations and ordinary operations are separate collections. Grammar
extraction preserves canonical names, aliases, instruction classes, ordered
operands, result shape, minimum word count, variable-width status, and literal
operand positions. Prose extraction classifies ``OpType*`` names but marks
their encoding incomplete rather than guessing an operand schema.

``literal_indices_known`` only says each literal operand position is known.
Whether a target instruction template accepts the result shape, variable word
count, or number of literal positions is target policy and stays in its private
catalogue.

Enumerants remain grouped by operand kind, and each enumerant likewise
preserves its aliases.

The Vulkan plane mirrors the registry's own taxonomy rather than a grouping of
this tool's. Types are collected under the ``category`` the registry gives them
— ``vk.structs``, ``vk.unions``, ``vk.enums``, ``vk.bitmasks``, ``vk.handles``,
``vk.func_pointers``, ``vk.base_types`` — because that set is closed and fixed
in ``registry.rnc``, so "what is missing" is answered by diffing against the
schema rather than by judgement. Alias chains are resolved with cycle checks,
while immediate alias targets remain available to templates.

Commands carry their exact parameter declarations, dispatch class, alias family
and requirement conditions, together with the recording constraints the registry
states: ``success_codes``, ``error_codes``, ``queues``, ``cmd_buffer_level``,
``render_pass``, ``video_coding`` and ``tasks``. Struct and union members carry
``limit_type``, ``optional``, ``len`` and the rest of the registry's member
attributes.

.. code-block:: toml

   [[vk.commands]]
   name = "vkWidgetAliasTEST"
   alias_of = "vkWidgetTEST"
   canonical_name = "vkWidgetTEST"
   return_type = "VkResult"
   dispatch = "device"
   protect = ""
   success_codes = "VK_SUCCESS"

   [[vk.commands.params]]
   name = "device"
   type_name = "VkDevice"
   declaration = "VkDevice device"

Guarding is a plane-level fact. An extension names a ``platform`` and the
registry's ``<platforms>`` table is the only place the macro is spelled, so
``vk.protect`` is resolved through that table; no ``<require>`` carries a
``protect`` attribute. Every command of one extension repeats the same value for
the convenience of a template looping over them.

An extension that ``supported`` does not list ``vulkan`` for is refused rather
than extracted: a disabled entry is spelled out in full and nothing else about
its contents distinguishes it.

Enumerators
~~~~~~~~~~~

Every enumerant the extension contributes is one entry in ``vk.enumerators``,
whatever it extends. The ``VkStructureType`` value tagging a struct is one of
these and is not privileged — extensions add far more to ``VkFormat``,
``VkResult`` and the flag-bits enums than they do to ``VkStructureType``. A
struct's ``stype`` names its tag; the value lives with the other enumerators.

``registry.rnc`` makes the four value forms mutually exclusive, so the value is
one tagged table rather than six optional fields: a template dispatches on
``kind`` once, and a document setting two of them fails to parse.

.. code-block:: toml

   [[vk.enumerators]]
   name = "VK_FORMAT_WIDGET_TEST"
   extends = "VkFormat"

   [vk.enumerators.value]
   kind = "offset"
   offset = 3
   ext_number = 232
   negative = false

.. list-table::
   :header-rows: 1
   :widths: 14 86

   * - ``kind``
     - Payload
   * - ``literal``
     - ``value``, as text — a name constant holds a quoted string.
   * - ``bitpos``
     - ``bitpos``, a bit position in a bitmask.
   * - ``offset``
     - ``offset`` within ``ext_number``'s reserved block, ``negative`` for
       ``dir="-"``.
   * - ``alias``
     - ``alias``, plus the ``canonical`` end of the chain.

The arithmetic for an ``offset`` stays with the target: a tree that registers
these already has a macro for the registry's formula, and emitting a computed
number would bypass it.

New enum *types* are separate. Their enumerants come from the registry's own
``<enums>`` block rather than from the extension's ``<require>``, so they sit
under ``vk.enums[].enumerants``.

The SPIR-V bridge
~~~~~~~~~~~~~~~~~

``vk.spirv`` carries the registry's ``<spirvextensions>`` and
``<spirvcapabilities>`` rows that name this extension. This is where the
registry itself states which SPIR-V tokens a Vulkan version, extension, feature
or property turns on, so it is read rather than reconstructed from naming
coincidences. Every ``enable`` route of a matching row is carried, not only the
one that matched: a capability is usually reachable several ways and which to
require is target policy.

.. code-block:: toml

   [[vk.spirv]]
   kind = "extension"
   name = "SPV_TEST_widget"

   [[vk.spirv.enables]]
   kind = "version"
   version = "VK_VERSION_1_1"

   [[vk.spirv.enables]]
   kind = "extension"
   extension = "VK_TEST_widget"

SPIR-V aliases stay on one numeric opcode record so a target registers exactly
one factory spelling. A sidecar may select either the canonical name or one
declared alias as that spelling. Vulkan commands instead retain one record per
public name because every name needs its own PFN, prototype, entry-point
metadata, and dispatch assignment; all records point at one canonical
implementation unless the sidecar overrides it.

The descriptor is an editable boundary. Correct it before generation when a
source note identifies missing or uncertain data.

Target catalogues
-----------------

Every TOML file below ``target/`` describes one tree:

.. code-block:: toml

   [target]
   name = "alpha"
   root = "{{ var.tree }}"
   plane = "spv"
   wrap = ["{{ var.frame }}"]

   [target.wrapper]
   open = "BEGIN {{ item }}\n"
   close = "END {{ item }}\n"
   open_pattern = '^BEGIN '

   [target.vars]
   tree = "~/work/tree-a"
   frame = "FRAME_{{ spv.feature }}"

.. list-table::
   :header-rows: 1
   :widths: 16 84

   * - Key
     - Meaning
   * - ``name``
     - Value selected by ``--target``.
   * - ``root``
     - Target directory template. Rule paths are relative to it.
   * - ``plane``
     - ``spv`` or ``vk``.
   * - ``wrap``
     - Default wrapper values for rules. Empty rendered values are ignored.
   * - ``wrapper``
     - Generic ``open`` and ``close`` templates for non-empty wrapper values.
   * - ``vars``
     - Free-form defaults that templates read as ``var.*``.

``root`` supports a leading ``~``. A missing explicitly selected root is an
error. When all catalogues are selected implicitly, a missing root is reported
and skipped because one machine need not contain every target tree.

Variables
~~~~~~~~~

.. code-block:: toml

   [target.vars]
   tree = "~/work/tree-a"
   label = "Support {{ spv.name }}"

Values supplied with ``--var NAME=VALUE`` override catalogue defaults:

.. code-block:: sh

   wits scaffold generate -d widget.toml -t alpha \
     --var tree=~/work/tree-a-topic

A template may read a variable only when the catalogue or the current command
defines it. An undefined ``var.*`` stops the command instead of rendering an
empty string. A value that has no sensible default can therefore be omitted
from ``[target.vars]``; any live rule that reads it then requires ``--var``.

Variable values may read descriptor fields, but they do not read other
variables. This keeps evaluation single-pass and rejects self-reference.

Per-run overlays
~~~~~~~~~~~~~~~~

Target-specific opcode and command choices do not belong in the specification
descriptor. Supply them in a sidecar:

.. code-block:: toml

   [target.alpha.spv.opcode.OpWidgetTEST]
   emit_name = "OpWidgetAliasTEST"
   template_base = "TargetInstructionBase"
   translation = "value"

   [target.beta.vk.command.vkWidgetTEST]
   implementation = "vkWidgetImplementationTEST"
   route = "device"
   entry_type = "@device"
   condition = "@dext(TEST_dependency)"

.. code-block:: sh

   wits scaffold generate -d widget.toml -t alpha --overlay widget-overlay.toml

SPIR-V entries may be keyed by their canonical name or one alias, but not both
in the same target. Vulkan command metadata is keyed by the public command
spelling. Unknown target names are rejected immediately; unknown opcode or
command names are rejected when that target is rendered. Overlay metadata is
merged into the record's ``meta`` table only for the named target.

The default Vulkan declaration route comes from the first dispatchable
parameter. A sidecar may select ``global``, ``instance``, ``physical_device``,
``device``, ``queue``, or ``command_buffer``; other values are rejected.
``entry_type`` overrides target metadata only and does not change the
registry-derived dispatch class.

Generic wrappers
~~~~~~~~~~~~~~~~

``target.wrap`` is a list of template values. Every non-empty value renders
``target.wrapper.open`` before the body and ``target.wrapper.close`` after it.
The openings use list order; closings use reverse order. The current value is
available as ``{{ item }}`` alongside the normal render context.

A rule inherits the target list when it omits ``wrap``. It can disable wrapping
or provide an explicit nested list:

.. code-block:: toml

   wrap = []

.. code-block:: toml

   wrap = ["{{ var.frame }}", "{{ var.outer }}"]

Because empty values are ignored, one run can remove a wrapper through the
ordinary variable mechanism:

.. code-block:: sh

   wits scaffold generate -d widget.toml -t alpha --var frame=

The tool assigns no lifecycle meaning to this operation. Variable names and
wrapper contents belong entirely to the private catalogue.

``open_pattern`` is optional. Sorted insertion uses it to identify consecutive
prefix lines attached to the entry below them and inserts before the entire
entry. This prevents a new item from landing between an existing prefix and
its keyed line without teaching the engine any particular prefix syntax.

Rules
-----

.. code-block:: toml

   [[rule]]
   what = "{{ kind.name }} entries"
   path = "data/items.txt"
   repeat = { over = "spv.kinds", as = "kind" }
   scope = ['^section {{ kind.name }}$']
   before = '^end$'
   body = """
   {%- for e in kind.enumerants %}
   {{ e.name }} = {{ e.value }}
   {%- endfor %}
   """

Every string is a Jinja template. Rules run in file order, and multiple rules
may edit the same file.

The rendered ``body`` is inserted exactly: no newline is added or removed. An
empty or whitespace-only body produces no edit. A wrapped body must end with a
newline so its closing text begins on its own line.

Conditional rules
~~~~~~~~~~~~~~~~~

``when`` controls whether a rule exists:

.. code-block:: toml

   when = "{{ spv.operations }}"

.. code-block:: toml

   when = "{{ var.frame }}"

A condition must be exactly one ``{{ … }}`` expression, and Jinja's own
truthiness decides it: empty strings, empty collections, ``false`` and ``0`` are
false. Modeled collections are always present and use ``[]`` when empty. An
unknown path is an error rather than false, so a misspelling cannot silently
remove a rule. No ``when`` means the rule always exists. Variables referenced
only by a rule whose condition is false are not required.

A rule that no longer applies is switched off by recording why, rather than by
writing a condition that cannot hold:

.. code-block:: toml

   disabled = "the tree generates this from its own tables now"

A non-empty ``disabled`` skips the rule before anything it names is rendered,
which matters because a rule is usually switched off precisely because what it
read has gone away. Keeping the rule with its reason attached is the point:
deleting it loses why the site is no longer scaffolded.

Two forms of iteration
~~~~~~~~~~~~~~~~~~~~~~

``repeat`` creates several edits, each with its own rendered anchor:

.. code-block:: toml

   repeat = { over = "spv.kinds", as = "kind" }

A Jinja loop inside ``body`` creates several lines within one edit. Use
``repeat`` when placement changes per item; use a body loop when all items
share one placement.

Anchors
~~~~~~~

Each rule declares exactly one placement shape:

.. list-table::
   :header-rows: 1
   :widths: 30 70

   * - Shape
     - Placement
   * - ``eof = true``
     - End of file.
   * - ``before = RE``
     - Before the first match.
   * - ``after_last = RE``
     - After the last match.
   * - ``scope = [RE, ...]`` with ``before = RE``
     - Before a terminator inside nested scopes.
   * - ``sorted = RE``
     - At the position implied by existing keyed lines.
   * - ``create = true``
     - Create a new file whose complete content is ``body``.

Patterns use multi-line mode, so ``^end$`` matches a complete line. ``scope``
patterns are followed in order, allowing an outer marker to distinguish
otherwise identical inner blocks.

For ``sorted``, capture group 1 is the key:

.. code-block:: toml

   sorted = '^([A-Z][A-Z0-9_]*) ='

An optional ``section`` pattern partitions keys; only entries in the new key's
partition are compared:

.. code-block:: toml

   section = '^([A-Z]+)_'

Digit runs compare numerically. When the existing list contradicts the
computed placement, the report names the entries that require review. The tool
never reorders existing text.

``create`` refuses to overwrite an existing file.

Filters
~~~~~~~

In addition to Jinja's built-ins. Everything but ``sha256`` comes from the
shared dialect in ``wits_util::jinja``, so a project config may use those too:

.. list-table::
   :header-rows: 1
   :widths: 24 76

   * - Filter
     - Result
   * - ``prefix(s)``
     - Prepend ``s`` to every string in a list.
   * - ``suffix(s)``
     - Append ``s`` to every string in a list.
   * - ``strip_prefix(s)``
     - Remove one leading occurrence of ``s``.
   * - ``pad(n)``
     - Right-pad to at least ``n`` characters without truncating.
   * - ``initials(n)``
     - Abbreviate an ``_``-separated name to ``n`` characters: one letter per
       word, then filling from the letters passed over. Refuses rather than
       returning a short result.
   * - ``sha256(n)``
     - Lowercase SHA-256 hex of the exact input, truncated to ``n`` characters.
   * - ``required(message)``
     - Turn an otherwise optional path into an error with ``message``.
   * - ``fail(message)``
     - Stop rendering immediately with ``message``.

``initials`` deliberately knows nothing about extension names: *which* words it
abbreviates is a tree's own convention and stays in that tree's catalogue. The
``sha256`` input is the string exactly as given, without case conversion or a
trailing newline.

The render context contains whichever of ``spv`` and ``vk`` are present,
``var``, ``target.name``, ``target.plane``, and any binding introduced by
``repeat``.

Generating changes
------------------

.. code-block:: sh

   wits scaffold generate -d widget.toml
   wits scaffold generate -d widget.toml -t alpha
   wits scaffold generate -d widget.toml -t alpha --overlay widget-overlay.toml
   wits scaffold generate -d widget.toml --write

A unified patch is the default. ``--write`` edits target trees directly; global
``--dry-run`` changes it back to patch output.

All edits are first resolved against in-memory buffers. Rules touching the
same file compose in catalogue order, and no file is written unless every edit
resolves successfully.

The command is intentionally not idempotent: running it twice inserts twice.
The patch is the review boundary, and ``git apply`` rejects stale context.

Checking catalogues
-------------------

.. code-block:: sh

   wits scaffold doctor -d widget.toml

``doctor`` renders every selected rule and resolves its anchor without
changing a tree. Unlike ``generate``, it reports every broken site in one run.
Create rules are checked for their own precondition: the destination must not
exist.

Any representative descriptor may be used. Supply the same overlay and
required variables that the live rules read.

Deliberate limits
-----------------

* The tool does not detect its own previous output.
* It does not invent values absent from a specification.
* It does not invent SPIR-V type internals, lowering bodies, or Vulkan command
  definitions. Catalogues may generate declarations and forwarding surfaces;
  implementations remain target work.
* It does not repair existing ordering.
* It does not guess a missing anchor, wrapper spelling, path, or variable.

Failure is preferred to plausible output at the wrong location.
