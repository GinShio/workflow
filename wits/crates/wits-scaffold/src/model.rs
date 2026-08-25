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
    /// Private catalogues give these keys meaning; the scaffold engine does not.
    #[serde(default)]
    pub meta: std::collections::BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub enumerants: Vec<Enumerant>,
}

/// One enumerant of an operand kind, named as both sources spell it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Enumerant {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub value: i64,
    /// For a `Capability`, the capabilities it implicitly declares; for another
    /// kind, the capabilities that enable it.
    #[serde(default)]
    pub requires: Vec<String>,
}

/// One canonical SPIR-V opcode and every spelling that aliases it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpvOpcode {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub value: i64,
    /// The grammar's instruction class. Prose extraction supplies
    /// `Type-Declaration` or `Unknown`.
    pub class: String,
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VkPlane {
    /// The registry spelling, e.g. `VK_VND_widget`.
    pub name: String,
    /// `VND_WIDGET`.
    pub feature: String,
    /// `vk_vnd_widget`.
    pub snake: String,
    pub extension_type: VkExtensionType,
    /// The registry's extension number.
    pub number: i64,
    pub spec_version: i64,
    #[serde(default)]
    pub author: String,
    /// The registry's raw `depends` expression, carried verbatim because its
    /// `+`/`,` grammar is the registry's to interpret, not ours.
    #[serde(default)]
    pub depends: String,
    #[serde(default)]
    pub structs: Vec<VkStruct>,
    #[serde(default)]
    pub commands: Vec<VkCommand>,
    #[serde(default)]
    pub type_aliases: Vec<VkAlias>,
    #[serde(default)]
    pub enum_aliases: Vec<VkAlias>,
    /// Feature members, each naming the struct that carries it.
    #[serde(default)]
    pub features: Vec<VkFeature>,
}

impl VkPlane {
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            feature: feature_stem(&name, "VK_"),
            snake: name.to_lowercase(),
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkStruct {
    /// `VkPhysicalDeviceWidgetFeaturesVND`.
    pub name: String,
    /// `VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND`.
    pub stype: String,
    /// Offset of `stype` within the extension's enum block, or the enumerator it
    /// aliases when no arithmetic value exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stype_offset: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stype_alias_of: Option<String>,
    /// True for a struct an implementation *answers* in a feature query rather
    /// than reads as an input.
    #[serde(default)]
    pub is_features: bool,
    #[serde(default)]
    pub is_properties: bool,
    #[serde(default)]
    pub members: Vec<VkMember>,
}

/// One member of an extension struct, past the common `sType`/`pNext` pair —
/// those are structural and every generated struct spells them the same way.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkMember {
    pub name: String,
    pub type_name: String,
    pub type_decl: String,
    #[serde(default)]
    pub suffix: String,
    pub declaration: String,
}

/// A queryable feature bit and the struct that reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkFeature {
    pub name: String,
    #[serde(rename = "struct")]
    pub struct_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkAlias {
    pub name: String,
    pub alias_of: String,
    pub canonical_name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VkRequirement {
    #[serde(default)]
    pub depends: String,
    #[serde(default)]
    pub protect: String,
    #[serde(default)]
    pub api: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VkDispatch {
    Global,
    Instance,
    PhysicalDevice,
    Device,
    Queue,
    CommandBuffer,
}

/// One public command spelling required by the selected extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VkCommand {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias_of: Option<String>,
    pub canonical_name: String,
    pub return_type: String,
    pub dispatch: VkDispatch,
    #[serde(default)]
    pub protect: String,
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
                aliases: Vec::new(),
                value: 5454,
                requires: vec!["BaseTEST".into()],
            }],
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
            operands: Vec::new(),
            capabilities: vec!["WidgetTEST".into()],
            encoding: SpvEncoding::default(),
            meta: Default::default(),
        });
        spv.kinds.push(KindGroup {
            name: "Alpha".into(),
            meta: std::collections::BTreeMap::from([(
                "label".into(),
                toml::Value::String("A".into()),
            )]),
            enumerants: vec![Enumerant {
                name: "WidgetTEST".into(),
                aliases: vec!["WidgetAliasTEST".into()],
                value: 5454,
                requires: vec!["BaseTEST".into()],
            }],
        });
        ext.spv = Some(spv);

        let text = to_toml(&ext).unwrap();
        let back = from_toml(&text).unwrap();
        let spv = back.spv.expect("spv plane survives the round trip");
        assert_eq!(spv.name, "SPV_TEST_widget");
        assert_eq!(spv.kinds[0].enumerants[0].value, 5454);
        assert_eq!(spv.operations[0].value, 5451);
        assert_eq!(spv.operations[0].aliases, ["OpWidgetAliasTEST"]);
        assert_eq!(spv.kinds[0].enumerants[0].aliases, ["WidgetAliasTEST"]);
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
