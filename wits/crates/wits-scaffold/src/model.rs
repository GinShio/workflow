//! The extension descriptor: what a scaffold needs to know, and nothing about
//! where it will be written.
//!
//! The descriptor is the seam between a fragile read of a specification and the
//! mechanical write into a target tree. Extraction may guess wrong — the prose
//! specs are hand-written and their table layout drifts — so the descriptor is
//! designed to be dumped, eyeballed, corrected by hand, and fed back in. Nothing
//! downstream of it ever looks at a spec again.
//!
//! It carries **two independent planes** because either specification may exist
//! without the other. Neither plane derives its name from its sibling.

use serde::{Deserialize, Serialize};

/// One extension, as far as scaffolding is concerned.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Extension {
    /// Anything extraction could not establish, carried through to the report so
    /// a guess never passes silently for a fact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spv: Option<SpvPlane>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vk: Option<VkPlane>,
}

impl Extension {
    /// Whether the plane a target declares it consumes is present. A target is
    /// skipped rather than half-applied when it is absent: half a scaffold is
    /// worse than none, because the gaps are silent.
    pub fn has_plane(&self, plane: Plane) -> bool {
        match plane {
            Plane::Spv => self.spv.is_some(),
            Plane::Vk => self.vk.is_some(),
        }
    }
}

/// Which specification plane a target draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Plane {
    Spv,
    Vk,
}

impl std::fmt::Display for Plane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Plane::Spv => "spv",
            Plane::Vk => "vk",
        })
    }
}

// ----------------------------------------------------------------------------
// SPIR-V plane
// ----------------------------------------------------------------------------

/// The SPIR-V tokens an extension introduces.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpvPlane {
    /// The registry spelling, e.g. `SPV_VND_widget`.
    pub name: String,
    /// `VND_WIDGET`.
    pub feature: String,
    /// `spv_vnd_widget`.
    pub snake: String,
    /// The registered vendor tag, `VND`, taken from the name.
    #[serde(default)]
    pub vendor: String,
    /// Grammar type declarations, kept separate because their result shape and
    /// target registration differ from ordinary operations.
    #[serde(default)]
    pub types: Vec<SpvOpcode>,
    /// Non-type opcodes introduced by the extension.
    #[serde(default)]
    pub operations: Vec<SpvOpcode>,
    /// One entry per operand kind the extension actually adds to. Grouping this
    /// way lets a rule fan out into one edit per kind.
    #[serde(default)]
    pub kinds: Vec<KindGroup>,
}

impl SpvPlane {
    /// One named group's enumerants, for tests that want to look at a single
    /// kind apart from the per-kind fan-out. The kind is a parameter rather than
    /// a literal because which kind matters differs by caller: parser tests name
    /// specification kinds, while planner tests use synthetic ones.
    ///
    /// Catalogues select a group in Jinja instead, so this is not part of the
    /// render context.
    #[cfg(test)]
    pub fn kind(&self, name: &str) -> &[Enumerant] {
        self.kinds
            .iter()
            .find(|group| group.name == name)
            .map(|group| group.enumerants.as_slice())
            .unwrap_or_default()
    }

    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            feature: feature_stem(&name, "SPV_"),
            snake: name.to_lowercase(),
            vendor: vendor_tag(&name),
            name,
            types: Vec::new(),
            operations: Vec::new(),
            kinds: Vec::new(),
        }
    }
}

/// One operand kind's enumerants plus arbitrary catalogue metadata copied from
/// `kinds.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KindGroup {
    /// The specification's operand-kind name.
    pub name: String,
    /// The grammar's own category: `BitEnum`, `ValueEnum`, `Id`, `Literal` or
    /// `Composite`. A target needs it to tell a flags declaration from a plain
    /// enum one, and it is a grammar fact, so it is read rather than configured.
    #[serde(default)]
    pub category: String,
    /// For a `Composite` kind, the kinds it is built from, in order.
    #[serde(default)]
    pub bases: Vec<String>,
    /// Private catalogues give these keys meaning; the scaffold engine does not.
    #[serde(default)]
    pub meta: std::collections::BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub enumerants: Vec<Enumerant>,
}

/// One enumerant of an operand kind, named as both sources spell it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Enumerant {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub value: i64,
    /// For a `Capability`, the capabilities it implicitly declares; for another
    /// kind, the capabilities that enable it.
    #[serde(default)]
    pub requires: Vec<String>,
    /// The SPIR-V version this entered core in, or `None` while it is reachable
    /// only through an extension.
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub last_version: String,
    /// Still provisional: the assignment may change before it is ratified.
    #[serde(default)]
    pub provisional: bool,
    /// Operands the enumerant itself carries — the id a `Bias` image operand
    /// takes, for instance. An encoder that ignores these writes a short
    /// instruction, so they are part of the enumerant rather than a detail of
    /// the kind.
    #[serde(default)]
    pub parameters: Vec<SpvOperand>,
}

/// One canonical SPIR-V opcode and every spelling that aliases it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpvOpcode {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub value: i64,
    /// The grammar's instruction class. Prose extraction supplies
    /// `Type-Declaration` or `Unknown`.
    pub class: String,
    /// The SPIR-V version this entered core in, or `None` while it is reachable
    /// only through an extension. A target that gates on core version needs it,
    /// and it cannot be derived from anything else in the record.
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub last_version: String,
    /// Still provisional: the opcode may change before it is ratified.
    #[serde(default)]
    pub provisional: bool,
    #[serde(default)]
    pub operands: Vec<SpvOperand>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub encoding: SpvEncoding,
    /// Target-specific values merged from an optional sidecar before rendering.
    #[serde(default)]
    pub meta: std::collections::BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpvOperand {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantifier: Option<String>,
}

/// Encoding facts the machine-readable grammar establishes without target
/// knowledge.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpvEncoding {
    pub has_result_type: bool,
    pub has_result_id: bool,
    pub min_word_count: usize,
    pub variable_word_count: bool,
    #[serde(default)]
    pub literal_operands: Vec<usize>,
    /// Whether every literal operand position is known statically.
    pub literal_indices_known: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incompatibility: Option<String>,
}

// ----------------------------------------------------------------------------
// Vulkan plane
// ----------------------------------------------------------------------------

/// The Vulkan API surface an extension introduces.
///
/// The collections mirror the registry's own `category` taxonomy rather than any
/// grouping of ours. That taxonomy is closed — Khronos fixes it in
/// `registry.rnc` — so the list cannot drift, and "what is missing" stays a
/// question you answer by diffing against the schema rather than by judgement.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VkPlane {
    /// The registry spelling, e.g. `VK_VND_widget`.
    pub name: String,
    /// `VND_WIDGET`.
    pub feature: String,
    /// `vk_vnd_widget`.
    pub snake: String,
    /// The registered vendor tag, `VND`, taken from the name.
    ///
    /// Not [`VkPlane::author`], which is who proposed the extension: 65 of the
    /// published extensions disagree between the two, and a few spell the author
    /// as a person's name.
    pub vendor: String,
    pub extension_type: VkExtensionType,
    /// The registry's extension number.
    pub number: i64,
    pub spec_version: i64,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub contact: String,
    /// The registry's raw `depends` expression, carried verbatim because its
    /// `+`/`,` grammar is the registry's to interpret, not ours.
    #[serde(default)]
    pub depends: String,
    /// The registry's `platform` name, empty for a portable extension.
    #[serde(default)]
    pub platform: String,
    /// The preprocessor symbol guarding a platform extension's declarations.
    ///
    /// This is a plane-level fact: `platform` names an entry in the registry's
    /// `<platforms>` table and that table is the only place the macro is spelled.
    /// Everything the extension declares is guarded by the same one.
    #[serde(default)]
    pub protect: String,
    /// The extension that supersedes this one, where the registry says so.
    #[serde(default)]
    pub promoted_to: String,
    #[serde(default)]
    pub deprecated_by: String,
    #[serde(default)]
    pub obsoleted_by: String,
    /// The registry's `specialuse` list: a warning that an extension is not for
    /// general use, carried verbatim because the set of uses is the registry's.
    #[serde(default)]
    pub special_use: String,
    /// Whether the extension is ratified, and for which APIs.
    #[serde(default)]
    pub ratified: String,
    #[serde(default)]
    pub provisional: bool,
    /// The registry's `nofeatures`: the extension adds no feature bits at all,
    /// which is distinct from adding some that this descriptor failed to find.
    #[serde(default)]
    pub no_features: bool,

    #[serde(default)]
    pub structs: Vec<VkStruct>,
    #[serde(default)]
    pub unions: Vec<VkUnion>,
    /// New enum *types*. Their enumerants come from the registry's own `<enums>`
    /// block, not from this extension's `<require>` — which is why they sit here
    /// rather than in [`VkPlane::enumerators`].
    #[serde(default)]
    pub enums: Vec<VkEnumType>,
    #[serde(default)]
    pub bitmasks: Vec<VkBitmask>,
    #[serde(default)]
    pub handles: Vec<VkHandle>,
    #[serde(default)]
    pub func_pointers: Vec<VkFuncPointer>,
    #[serde(default)]
    pub base_types: Vec<VkBaseType>,
    /// Enumerants this extension adds to enums that already exist, whatever they
    /// are. The `VkStructureType` entry tagging a struct is one of these; so is
    /// every `VkFormat`, `VkResult` or flag bit the extension contributes.
    #[serde(default)]
    pub enumerators: Vec<VkEnumerator>,
    #[serde(default)]
    pub commands: Vec<VkCommand>,
    #[serde(default)]
    pub type_aliases: Vec<VkAlias>,
    /// Feature members, each naming the struct that carries it.
    #[serde(default)]
    pub features: Vec<VkFeature>,
    /// What the registry's own `<spirvextensions>`/`<spirvcapabilities>` tables
    /// say this extension enables on the SPIR-V side.
    #[serde(default)]
    pub spirv: Vec<VkSpirvRequirement>,
}

impl VkPlane {
    /// Whether the extension contributes nothing a target could register.
    ///
    /// Every collection counts, not just the ones with a C declaration behind
    /// them: an extension whose whole Vulkan surface is a handful of `VkFormat`
    /// enumerants, or nothing but a `<spirvextension>` row, is still an
    /// extension a tree has to know about.
    pub fn is_empty(&self) -> bool {
        self.structs.is_empty()
            && self.unions.is_empty()
            && self.enums.is_empty()
            && self.bitmasks.is_empty()
            && self.handles.is_empty()
            && self.func_pointers.is_empty()
            && self.base_types.is_empty()
            && self.enumerators.is_empty()
            && self.commands.is_empty()
            && self.type_aliases.is_empty()
            && self.features.is_empty()
            && self.spirv.is_empty()
    }

    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            feature: feature_stem(&name, "VK_"),
            snake: name.to_lowercase(),
            vendor: vendor_tag(&name),
            name,
            extension_type: VkExtensionType::Device,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VkExtensionType {
    Instance,
    #[default]
    Device,
}

/// A struct the extension adds, with the `VkStructureType` that tags it.
///
/// The tagging enumerator's *value* is not here: it is an ordinary entry in
/// [`VkPlane::enumerators`] with `extends = "VkStructureType"`, the same as
/// every other enumerant the extension contributes. `stype` is the link.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkStruct {
    /// `VkPhysicalDeviceWidgetFeaturesVND`.
    pub name: String,
    /// `VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND`.
    pub stype: String,
    /// True for a struct an implementation *answers* in a feature query rather
    /// than reads as an input.
    #[serde(default)]
    pub is_features: bool,
    #[serde(default)]
    pub is_properties: bool,
    /// The registry's `returnedonly`: the application never fills this in.
    #[serde(default)]
    pub returned_only: bool,
    /// Everything the struct may be chained onto, verbatim.
    #[serde(default)]
    pub struct_extends: String,
    #[serde(default)]
    pub members: Vec<VkMember>,
}

/// A union the extension adds. Members are spelled exactly as for a struct;
/// what differs is the declaration keyword, which is the target's to write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkUnion {
    pub name: String,
    #[serde(default)]
    pub returned_only: bool,
    #[serde(default)]
    pub members: Vec<VkMember>,
}

/// One member of an extension struct or union, past the common `sType`/`pNext`
/// pair — those are structural and every generated struct spells them the same
/// way.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkMember {
    pub name: String,
    pub type_name: String,
    pub type_decl: String,
    #[serde(default)]
    pub suffix: String,
    pub declaration: String,
    /// The registry's `optional`, a comma-separated list matching the pointer
    /// depth, so it is carried as text rather than reduced to a boolean.
    #[serde(default)]
    pub optional: String,
    /// How a properties member combines across implementations: `max`, `min`,
    /// `bitmask`, `exact`, `noauto` and so on. Empty for a non-properties member.
    #[serde(default)]
    pub limit_type: String,
    /// The member giving this one's element count, for an array.
    #[serde(default)]
    pub len: String,
    /// A C expression for the length where `len` cannot express it.
    #[serde(default)]
    pub alt_len: String,
    #[serde(default)]
    pub no_auto_validity: String,
    #[serde(default)]
    pub extern_sync: String,
    /// The `VkObjectType` a handle member holds, where the registry says.
    #[serde(default)]
    pub object_type: String,
    /// For a union member, the enumerator selecting it; on the selecting member,
    /// `selector` names the member it governs.
    #[serde(default)]
    pub selector: String,
    #[serde(default)]
    pub selection: String,
    /// The registry's `values`, the enumerants the member may hold. The
    /// published registry sets it only on `sType`, which `members` leaves out
    /// because its value is already the struct's `stype`, so it is empty on
    /// every member today.
    #[serde(default)]
    pub values: String,
    #[serde(default)]
    pub api: String,
    #[serde(default)]
    pub deprecated: String,
}

/// A new enum type, with the enumerants the registry files under it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkEnumType {
    pub name: String,
    /// The registry's `<enums type=>`: `enum` for a value enum, `bitmask` for a
    /// flag-bits enum.
    #[serde(default)]
    pub kind: String,
    /// 32 unless the registry widens it.
    #[serde(default)]
    pub bitwidth: i64,
    #[serde(default)]
    pub enumerants: Vec<VkEnumerator>,
}

/// A `VkFlags` typedef and the flag-bits enum it draws from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkBitmask {
    pub name: String,
    /// The `*FlagBits` enum supplying the values, empty for a bitmask with none
    /// defined yet.
    #[serde(default)]
    pub requires: String,
    /// The registry's `bitvalues`, used instead of `requires` for the 64-bit
    /// form.
    #[serde(default)]
    pub bit_values: String,
    #[serde(default)]
    pub api: String,
}

/// A new object handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkHandle {
    pub name: String,
    /// False for the non-dispatchable form, which the registry spells by using
    /// a different definition macro rather than by an attribute.
    pub dispatchable: bool,
    /// The handle this one is created from.
    #[serde(default)]
    pub parent: String,
    /// The `VkObjectType` enumerator naming this handle.
    #[serde(default)]
    pub object_type_enum: String,
}

/// A function-pointer typedef, carried as the registry spells it because its
/// signature is C text rather than structured content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkFuncPointer {
    pub name: String,
    pub declaration: String,
    #[serde(default)]
    pub requires: String,
}

/// A scalar or forward-declared type the extension brings in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkBaseType {
    pub name: String,
    pub declaration: String,
}

/// One enumerant an extension contributes to an enum that already exists.
///
/// At least one of `value` and `alias` is present: an `<enum>` that spells
/// neither only refers to an enumerant defined elsewhere, and is not recorded.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VkEnumerator {
    pub name: String,
    /// The enum being extended. Empty for a bare constant such as
    /// `VK_..._SPEC_VERSION`, which extends nothing.
    #[serde(default)]
    pub extends: String,
    /// `uint8_t`, `uint32_t`, `uint64_t` or `float`, where the registry pins it.
    #[serde(default)]
    pub type_name: String,
    #[serde(default)]
    pub api: String,
    /// An additional guard around this one enumerator, independent of the
    /// extension's platform.
    #[serde(default)]
    pub protect: String,
    /// `aliased`, `unused` or `true`, where the registry marks it legacy.
    #[serde(default)]
    pub deprecated: String,
    /// How the registry spells the value. Absent for an enumerator that is
    /// only an alias.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<VkEnumValue>,
    /// The enumerator this one is identical to, where the registry says so.
    ///
    /// Read apart from `value` because `registry.rnc` lets an alias stand alone
    /// or accompany a `value` or `bitpos`. Where both are present the header
    /// generator emits the value (Vulkan-Docs `scripts/generator.py`,
    /// `enumToValue`); which one a tree registers is its catalogue's choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<VkEnumAlias>,
}

/// How the registry spells an enumerator's value.
///
/// `registry.rnc` never lets two of these appear together, so this is one
/// tagged value rather than five optional fields: a template dispatches on
/// `kind` once instead of probing each field in turn, and a document setting
/// two of them fails to parse instead of silently taking whichever the reader
/// checks first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VkEnumValue {
    /// A literal, as text: a constant such as `VK_..._EXTENSION_NAME` holds a
    /// quoted string here, so narrowing this to an integer would lose it.
    Literal { value: String },
    /// A bit position within a bitmask.
    Bitpos { bitpos: i64 },
    /// An offset within an extension's reserved block.
    ///
    /// The arithmetic stays with the target: every tree that registers these
    /// already has its own macro for the registry's formula, and emitting a
    /// computed number instead would bypass it. `ext_number` is the defining
    /// extension's unless the enumerator borrows another extension's block.
    Offset {
        offset: i64,
        ext_number: i64,
        /// `dir="-"`, placing the enumerator below the block base rather than
        /// above it. `VkResult` error codes are why it exists.
        #[serde(default)]
        negative: bool,
    },
}

/// The enumerator an alias is identical to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VkEnumAlias {
    /// The enumerator the registry names, which may itself be an alias.
    pub of: String,
    /// The end of that chain, resolved here because following it needs the
    /// whole document and a cycle check.
    pub canonical: String,
}

/// A queryable feature bit and the struct that reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkFeature {
    pub name: String,
    #[serde(rename = "struct")]
    pub struct_name: String,
}

/// One entry of the registry's SPIR-V tables, as it concerns this extension.
///
/// `<spirvextensions>` and `<spirvcapabilities>` are where the registry itself
/// states which SPIR-V tokens a Vulkan version, extension, feature or property
/// turns on. It is the only place the two planes are linked, so it is read
/// rather than reconstructed from naming coincidences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkSpirvRequirement {
    /// Whether the name is a `SPV_` extension or a SPIR-V capability.
    pub kind: VkSpirvKind,
    pub name: String,
    /// Every condition the registry lists, not only the one that selected this
    /// entry: a capability is commonly reachable more than one way, and which
    /// route a target prefers is the target's decision.
    #[serde(default)]
    pub enables: Vec<VkSpirvEnable>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VkSpirvKind {
    Extension,
    Capability,
}

/// One way a SPIR-V token becomes available, in the registry's own four forms.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VkSpirvEnable {
    /// A core version enables it.
    Version { version: String },
    /// A Vulkan extension enables it.
    Extension { extension: String },
    /// A feature bit enables it.
    Feature {
        #[serde(rename = "struct")]
        struct_name: String,
        feature: String,
        #[serde(default)]
        requires: String,
        #[serde(default)]
        alias: String,
    },
    /// A property member holding a given value enables it.
    Property {
        property: String,
        member: String,
        value: String,
        #[serde(default)]
        requires: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkAlias {
    pub name: String,
    pub alias_of: String,
    pub canonical_name: String,
}

/// One `<require>` block's condition, as it applies to what that block names.
///
/// A `<require>` carries no `protect`: guarding is a property of the extension's
/// platform, not of any one requirement block. See [`VkPlane::protect`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VkRequirement {
    #[serde(default)]
    pub depends: String,
    #[serde(default)]
    pub api: String,
}

/// Which handle a command dispatches through, read off its first parameter.
///
/// `Global` is the default because it is the no-handle case: a command whose
/// first parameter is not a dispatchable handle is dispatched globally.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VkDispatch {
    #[default]
    Global,
    Instance,
    PhysicalDevice,
    Device,
    Queue,
    CommandBuffer,
}

/// One public command spelling required by the selected extension.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VkCommand {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias_of: Option<String>,
    pub canonical_name: String,
    pub return_type: String,
    pub dispatch: VkDispatch,
    /// A copy of [`VkPlane::protect`], so a template looping over commands can
    /// guard each declaration without reaching back out to the plane. Every
    /// command of one extension carries the same value.
    #[serde(default)]
    pub protect: String,
    /// The `VkResult` values the command may return on success, verbatim.
    #[serde(default)]
    pub success_codes: String,
    #[serde(default)]
    pub error_codes: String,
    /// Which queue families may record the command.
    #[serde(default)]
    pub queues: String,
    /// `primary`, `secondary`, or both.
    #[serde(default)]
    pub cmd_buffer_level: String,
    /// Whether the command is legal inside a render pass, outside, or both.
    #[serde(default)]
    pub render_pass: String,
    /// Whether the command is legal inside a video coding scope.
    #[serde(default)]
    pub video_coding: String,
    /// The task types the command performs: `action`, `state`, `synchronization`
    /// and so on.
    #[serde(default)]
    pub tasks: String,
    #[serde(default)]
    pub conditional_rendering: String,
    /// The command that replaces this one, where the registry says so.
    #[serde(default)]
    pub superseded_by: String,
    #[serde(default)]
    pub params: Vec<VkParam>,
    #[serde(default)]
    pub requirements: Vec<VkRequirement>,
    #[serde(default)]
    pub meta: std::collections::BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VkParam {
    pub name: String,
    pub type_name: String,
    /// C spelling before the parameter name.
    pub type_decl: String,
    /// C spelling after the parameter name, such as an array extent.
    #[serde(default)]
    pub suffix: String,
    /// Complete C parameter spelling reconstructed from the registry's mixed
    /// content.
    pub declaration: String,
    /// The registry's `optional`, one entry per pointer level, so it is text
    /// rather than a boolean.
    #[serde(default)]
    pub optional: String,
    /// The parameter giving this one's element count, for an array.
    #[serde(default)]
    pub len: String,
    /// A C expression for the length where `len` cannot express it.
    #[serde(default)]
    pub alt_len: String,
    /// Which of the parameter's handles the caller must externally synchronise.
    #[serde(default)]
    pub extern_sync: String,
    #[serde(default)]
    pub no_auto_validity: String,
    /// The `VkObjectType` a handle parameter holds, where the registry says.
    #[serde(default)]
    pub object_type: String,
    /// The structures a `void*` parameter may legally point at.
    #[serde(default)]
    pub valid_structs: String,
    #[serde(default)]
    pub stride: String,
    #[serde(default)]
    pub api: String,
}

// ----------------------------------------------------------------------------
// Descriptor serialisation
// ----------------------------------------------------------------------------

/// Render the descriptor as TOML, matching every other file in the wits config
/// tree. Fields are declared scalars-first so the emitted document never puts a
/// value after a table, which TOML forbids.
pub fn to_toml(ext: &Extension) -> anyhow::Result<String> {
    Ok(toml::to_string_pretty(ext)?)
}

pub fn from_toml(text: &str) -> anyhow::Result<Extension> {
    Ok(toml::from_str(text)?)
}

/// `SPV_VND_widget` -> `VND_WIDGET`.
fn feature_stem(name: &str, plane_prefix: &str) -> String {
    name.strip_prefix(plane_prefix)
        .unwrap_or(name)
        .to_uppercase()
}

/// `SPV_VND_widget` -> `VND`. Both registries spell a name the same way, so one
/// reading serves both planes. Empty for a name that does not follow it.
fn vendor_tag(name: &str) -> String {
    name.split('_').nth(1).unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spv_names_derive_from_the_registry_spelling() {
        let plane = SpvPlane::new("SPV_TEST_widget");
        assert_eq!(plane.feature, "TEST_WIDGET");
        assert_eq!(plane.snake, "spv_test_widget");
    }

    #[test]
    fn vk_names_derive_independently_of_any_spv_plane() {
        let plane = VkPlane::new("VK_TEST_widget");
        assert_eq!(plane.feature, "TEST_WIDGET");
        assert_eq!(plane.snake, "vk_test_widget");
    }

    #[test]
    fn an_unprefixed_name_is_left_alone_rather_than_mangled() {
        // Hand-written descriptors do appear; a name that does not carry the
        // plane prefix still has to produce something usable.
        assert_eq!(feature_stem("TEST_thing", "SPV_"), "TEST_THING");
    }

    #[test]
    fn a_named_kind_group_is_selectable() {
        let mut plane = SpvPlane::new("SPV_TEST_x");
        plane.kinds.push(KindGroup {
            name: "Alpha".into(),
            meta: std::collections::BTreeMap::from([(
                "label".into(),
                toml::Value::String("A".into()),
            )]),
            enumerants: vec![Enumerant {
                name: "WidgetTEST".into(),
                value: 5454,
                requires: vec!["BaseTEST".into()],
                ..Default::default()
            }],
            ..Default::default()
        });
        assert_eq!(plane.kind("Alpha").len(), 1);
        assert_eq!(plane.kind("Alpha")[0].name, "WidgetTEST");
    }

    #[test]
    fn a_plane_absent_is_reported_not_guessed() {
        let ext = Extension {
            spv: Some(SpvPlane::new("SPV_TEST_x")),
            ..Default::default()
        };
        assert!(ext.has_plane(Plane::Spv));
        assert!(!ext.has_plane(Plane::Vk));
    }

    #[test]
    fn descriptor_round_trips_through_toml() {
        let mut ext = Extension::default();
        let mut spv = SpvPlane::new("SPV_TEST_widget");
        spv.operations.push(SpvOpcode {
            name: "OpWidgetTEST".into(),
            aliases: vec!["OpWidgetAliasTEST".into()],
            value: 5451,
            class: "Arithmetic".into(),
            capabilities: vec!["WidgetTEST".into()],
            ..Default::default()
        });
        spv.kinds.push(KindGroup {
            name: "Alpha".into(),
            category: "ValueEnum".into(),
            meta: std::collections::BTreeMap::from([(
                "label".into(),
                toml::Value::String("A".into()),
            )]),
            enumerants: vec![Enumerant {
                name: "WidgetTEST".into(),
                aliases: vec!["WidgetAliasTEST".into()],
                value: 5454,
                requires: vec!["BaseTEST".into()],
                parameters: vec![SpvOperand {
                    kind: "IdRef".into(),
                    name: Some("Value".into()),
                    quantifier: None,
                }],
                ..Default::default()
            }],
            ..Default::default()
        });
        ext.spv = Some(spv);

        let text = to_toml(&ext).unwrap();
        let back = from_toml(&text).unwrap();
        let spv = back.spv.expect("spv plane survives the round trip");
        assert_eq!(spv.name, "SPV_TEST_widget");
        assert_eq!(spv.kinds[0].enumerants[0].value, 5454);
        assert_eq!(spv.operations[0].value, 5451);
        assert_eq!(spv.operations[0].aliases, ["OpWidgetAliasTEST"]);
        assert_eq!(spv.kinds[0].category, "ValueEnum");
        // An enumerant's own operands ride with it: an encoder that loses them
        // emits a short instruction, which no later stage can detect.
        assert_eq!(spv.kinds[0].enumerants[0].parameters[0].kind, "IdRef");
        assert_eq!(spv.kinds[0].enumerants[0].aliases, ["WidgetAliasTEST"]);
    }

    #[test]
    fn each_enumerator_form_survives_the_document() {
        // The value forms and the alias are what a catalogue dispatches on, so
        // a round trip that quietly collapsed one into another — or dropped the
        // alias riding beside a value — would show up as a wrong enumerator in a
        // target tree rather than as an error here.
        let alias = |of: &str| {
            Some(VkEnumAlias {
                of: of.into(),
                canonical: of.into(),
            })
        };
        let mut vk = VkPlane::new("VK_TEST_widget");
        vk.enumerators = vec![
            VkEnumerator {
                name: "VK_TEST_WIDGET_EXTENSION_NAME".into(),
                value: Some(VkEnumValue::Literal {
                    value: "\"VK_TEST_widget\"".into(),
                }),
                ..Default::default()
            },
            VkEnumerator {
                name: "VK_WIDGET_USAGE_FAST_BIT_TEST".into(),
                extends: "VkWidgetUsageFlagBitsTEST".into(),
                value: Some(VkEnumValue::Bitpos { bitpos: 7 }),
                ..Default::default()
            },
            VkEnumerator {
                name: "VK_ERROR_WIDGET_LOST_TEST".into(),
                extends: "VkResult".into(),
                value: Some(VkEnumValue::Offset {
                    offset: 1,
                    ext_number: 232,
                    negative: true,
                }),
                ..Default::default()
            },
            VkEnumerator {
                name: "VK_STRUCTURE_TYPE_WIDGET_ALIAS_TEST".into(),
                extends: "VkStructureType".into(),
                alias: alias("VK_STRUCTURE_TYPE_WIDGET_TEST"),
                ..Default::default()
            },
            VkEnumerator {
                name: "VK_WIDGET_USAGE_QUICK_BIT_TEST".into(),
                extends: "VkWidgetUsageFlagBitsTEST".into(),
                value: Some(VkEnumValue::Bitpos { bitpos: 7 }),
                alias: alias("VK_WIDGET_USAGE_FAST_BIT_TEST"),
                ..Default::default()
            },
        ];
        let expected = vk.enumerators.clone();

        let text = to_toml(&Extension {
            vk: Some(vk),
            ..Default::default()
        })
        .unwrap();
        // The tag is what a template branches on, so it has to be in the
        // document rather than implied by which field is present.
        assert!(text.contains("kind = \"offset\""), "got:\n{text}");

        let back = from_toml(&text).unwrap().vk.expect("the vk plane");
        assert_eq!(back.enumerators.len(), expected.len());
        for (wrote, read) in expected.iter().zip(&back.enumerators) {
            assert_eq!(wrote.value, read.value, "{}", wrote.name);
            assert_eq!(wrote.alias, read.alias, "{}", wrote.name);
        }
    }

    #[test]
    fn empty_modeled_collections_remain_explicit_in_the_document() {
        let ext = Extension {
            spv: Some(SpvPlane::new("SPV_TEST_empty")),
            ..Default::default()
        };
        let text = to_toml(&ext).unwrap();
        assert!(text.contains("types = []"), "got:\n{text}");
        assert!(text.contains("operations = []"), "got:\n{text}");
        assert!(text.contains("kinds = []"), "got:\n{text}");
    }
}
