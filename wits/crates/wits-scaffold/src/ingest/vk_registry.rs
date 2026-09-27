//! Select one extension out of the Vulkan registry, `vk.xml`.
//!
//! The registry is machine-readable and complete, so nothing here is heuristic.
//!
//! ## What is read, and how the completeness question is answered
//!
//! The reader follows the registry's own taxonomy: `<type>` is dispatched on its
//! `category`, an `<enum>` value on which of the four mutually exclusive forms
//! `registry.rnc` allows, and nothing is grouped in a way the schema does not.
//! That is deliberate — it makes "is anything missing" a question you settle by
//! diffing this module against the schema, rather than one that needs a survey
//! of the document.
//!
//! ## Three indirections worth spelling out
//!
//! A struct names its `VkStructureType` *inside itself*, on the `sType` member's
//! `values` attribute. Following that pairs the two exactly, where deriving one
//! name from the other by case-mangling would fail on the irregular ones. The
//! enumerator itself is an ordinary entry in the extension's `<require>` block
//! and is read as one, like every other enumerant.
//!
//! A platform extension's guard macro is spelled only in `<platforms>`; the
//! extension names a platform, and no `<require>` carries a `protect` attribute
//! at all. See [`platform_protect`].
//!
//! Which extension a type belongs to is not written down anywhere. The header
//! generator settles it by the order it emits blocks in, and a `<require>` list
//! both names types another block declares and omits types its own block does,
//! so the list cannot answer it. See [`declared_by`].

use anyhow::{bail, Context, Result};
use roxmltree::{Document, Node};

use crate::model::{
    VkAlias, VkBaseType, VkBitmask, VkCommand, VkDispatch, VkEnumType, VkEnumValue, VkEnumerator,
    VkExtensionType, VkFeature, VkFuncPointer, VkHandle, VkMember, VkParam, VkPlane, VkRequirement,
    VkSpirvEnable, VkSpirvKind, VkSpirvRequirement, VkStruct, VkUnion,
};

/// Extract `name`'s API surface from a registry document.
pub fn extract(text: &str, name: &str) -> Result<(VkPlane, Vec<String>)> {
    let doc = Document::parse(text).context("vk.xml is not well-formed XML")?;
    let mut notes = Vec::new();

    let extension = doc
        .descendants()
        .find(|node| node.has_tag_name("extension") && node.attribute("name") == Some(name))
        .with_context(|| {
            format!(
                "no <extension name=\"{name}\"> in this registry — check the spelling, or \
                     write the descriptor by hand if the extension is not public yet"
            )
        })?;

    // `supported` lists the APIs an extension belongs to. A disabled or
    // Vulkan SC-only one is spelled out in full here, so nothing about its
    // contents distinguishes it from one worth scaffolding — only this does.
    let supported = supported_apis(extension);
    if !names_vulkan(supported) {
        bail!("{name} is supported = \"{supported}\", which does not include vulkan");
    }

    let mut plane = VkPlane::new(name);
    plane.extension_type = match extension.attribute("type").unwrap_or("device") {
        "instance" => VkExtensionType::Instance,
        "device" => VkExtensionType::Device,
        other => bail!("{name} has unknown extension type '{other}'"),
    };
    plane.number = extension
        .attribute("number")
        .context("extension has no number")?
        .parse()
        .with_context(|| format!("{name} has a non-numeric extension number"))?;
    plane.author = extension.attribute("author").unwrap_or_default().to_owned();
    plane.contact = extension
        .attribute("contact")
        .unwrap_or_default()
        .to_owned();
    plane.depends = extension
        .attribute("depends")
        .unwrap_or_default()
        .to_owned();
    plane.promoted_to = text_attribute(extension, "promotedto");
    plane.deprecated_by = text_attribute(extension, "deprecatedby");
    plane.obsoleted_by = text_attribute(extension, "obsoletedby");
    plane.special_use = text_attribute(extension, "specialuse");
    plane.ratified = text_attribute(extension, "ratified");
    plane.provisional = extension.attribute("provisional") == Some("true");
    plane.no_features = extension.attribute("nofeatures") == Some("true");
    plane.platform = extension
        .attribute("platform")
        .unwrap_or_default()
        .to_owned();
    if !plane.platform.is_empty() {
        plane.protect = platform_protect(&doc, &plane.platform).with_context(|| {
            format!(
                "{name} names platform '{}', which <platforms> does not define",
                plane.platform
            )
        })?;
    }

    // `<enum value="1" name="VK_..._SPEC_VERSION">`
    plane.spec_version = extension
        .descendants()
        .filter(|node| node.has_tag_name("enum"))
        .find(|node| {
            node.attribute("name")
                .is_some_and(|n| n.ends_with("_SPEC_VERSION"))
        })
        .and_then(|node| node.attribute("value"))
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);

    let requirements: Vec<_> = vulkan_requirements(extension).collect();

    plane.features = requirements
        .iter()
        .flat_map(|requirement| requirement.descendants())
        .filter(|node| node.has_tag_name("feature"))
        .filter_map(|node| {
            Some(VkFeature {
                name: node.attribute("name")?.to_owned(),
                struct_name: node.attribute("struct")?.to_owned(),
            })
        })
        .collect();

    let protect = plane.protect.clone();
    for requirement in &requirements {
        let condition = VkRequirement {
            depends: requirement
                .attribute("depends")
                .unwrap_or_default()
                .to_owned(),
            api: requirement.attribute("api").unwrap_or_default().to_owned(),
        };
        for command_name in requirement
            .children()
            .filter(|node| node.has_tag_name("command"))
            .filter_map(|node| node.attribute("name"))
        {
            let command = command_of(&doc, command_name, condition.clone(), &protect)?;
            match plane
                .commands
                .iter_mut()
                .find(|existing| existing.name == command.name)
            {
                // One command can be required by several blocks, each under its
                // own condition. Keep a single record and collect the conditions.
                Some(existing) => existing.requirements.extend(command.requirements),
                None => plane.commands.push(command),
            }
        }
    }

    // Every enumerant the extension contributes, to whatever enum. The
    // `VkStructureType` entry tagging a struct is one of these and gets no
    // special handling: an extension adds far more entries to VkFormat,
    // VkResult and the flag-bits enums than it does sType values.
    for node in requirements
        .iter()
        .flat_map(|requirement| requirement.descendants())
        .filter(|node| node.has_tag_name("enum"))
    {
        match enumerator_of(&doc, node, plane.number)? {
            Some(enumerator) => plane.enumerators.push(enumerator),
            // The registry also uses <enum> to *reference* an existing
            // enumerant, with no value of any kind. Nothing to record.
            None => continue,
        }
    }
    plane.enumerators = dedupe_enumerators(plane.enumerators, &mut notes);

    // The tags this extension gives a value to, which are the only ones it can
    // register a struct through.
    let introduced: std::collections::BTreeSet<&str> = plane
        .enumerators
        .iter()
        .map(|enumerator| enumerator.name.as_str())
        .collect();

    let mut structs = Vec::new();
    for (referenced, type_node) in declared_by(&doc, name) {
        if let Some(alias_of) = type_node.attribute("alias") {
            plane.type_aliases.push(VkAlias {
                name: referenced.to_owned(),
                alias_of: alias_of.to_owned(),
                canonical_name: canonical_type_name(&doc, referenced)?,
            });
            continue;
        }
        // Dispatch on the registry's own taxonomy. `include` and `define` are
        // the two categories with nothing to generate: one names a system
        // header, the other is preprocessor text the headers already carry.
        match type_node.attribute("category").unwrap_or_default() {
            "struct" => {
                let record = struct_of(type_node, referenced)?;
                // A chainable struct reaches a tree through its tag, so a tag
                // with no value leaves it unregisterable — say so rather than
                // emitting a declaration nothing can reference. The tag may be
                // named here without a value, or not named at all when the
                // struct came in as a dependency of one this extension lists.
                // A struct with no `sType` has no such gate and is simply
                // declared.
                if record.stype.is_empty() || introduced.contains(record.stype.as_str()) {
                    structs.push(record);
                } else {
                    notes.push(format!(
                        "{referenced}: its {} has no value in this extension's require block, so \
                         the struct cannot be registered",
                        record.stype
                    ));
                }
            }
            "union" => plane.unions.push(VkUnion {
                name: referenced.to_owned(),
                returned_only: type_node.attribute("returnedonly") == Some("true"),
                members: members_of(type_node)?,
            }),
            "enum" => plane.enums.push(enum_type_of(&doc, referenced)?),
            "bitmask" => plane.bitmasks.push(VkBitmask {
                name: referenced.to_owned(),
                requires: text_attribute(type_node, "requires"),
                bit_values: text_attribute(type_node, "bitvalues"),
                api: text_attribute(type_node, "api"),
            }),
            "handle" => plane.handles.push(VkHandle {
                name: referenced.to_owned(),
                // The registry marks dispatchability by which definition macro
                // it uses, not by an attribute.
                dispatchable: mixed_text(type_node).contains("VK_DEFINE_HANDLE"),
                parent: text_attribute(type_node, "parent"),
                object_type_enum: text_attribute(type_node, "objtypeenum"),
            }),
            "funcpointer" => plane.func_pointers.push(VkFuncPointer {
                name: referenced.to_owned(),
                declaration: mixed_text(type_node),
                requires: text_attribute(type_node, "requires"),
            }),
            "basetype" => plane.base_types.push(VkBaseType {
                name: referenced.to_owned(),
                declaration: mixed_text(type_node),
            }),
            _ => continue,
        }
    }
    plane.structs = structs;

    plane.spirv = spirv_requirements(&doc, name);

    if plane.is_empty() {
        bail!("{name} adds no modeled API surface; there is nothing to scaffold");
    }
    Ok((plane, notes))
}

/// Whether a registry API list — an `api` or `supported` attribute — names
/// Vulkan.
fn names_vulkan(apis: &str) -> bool {
    apis.split(',').any(|api| api == "vulkan")
}

/// An extension's `supported` list. Absent is read as Vulkan, for the
/// hand-written documents that omit it.
fn supported_apis<'a>(extension: Node<'a, 'a>) -> &'a str {
    extension.attribute("supported").unwrap_or("vulkan")
}

/// A feature's or extension's `<require>` blocks that apply to Vulkan.
fn vulkan_requirements<'a>(block: Node<'a, 'a>) -> impl Iterator<Item = Node<'a, 'a>> {
    block.children().filter(|node| {
        node.has_tag_name("require") && node.attribute("api").is_none_or(names_vulkan)
    })
}

/// The types the Vulkan header declares under `extension`, in the order its
/// generator reaches them.
///
/// The registry never says outright who *defines* a type; its header generator
/// decides, and this repeats its walk (Vulkan-Docs `scripts/reg.py`,
/// `generateRequiredInterface` and `generateFeature`). Blocks are visited in
/// [`emission_rank`] order, and a type is declared under the first block that
/// requires it — listed as a `<type>`, or used by a `<command>` it lists —
/// together with everything that type depends on: whatever its `alias`,
/// `requires` and `bitvalues` name, and every `<type>` inside its definition.
///
/// A `<require>` list therefore cannot answer the question alone. It names
/// types another block declared first, and it omits types its own block
/// declares as dependencies.
fn declared_by<'a>(doc: &'a Document<'a>, extension: &str) -> Vec<(&'a str, Node<'a, 'a>)> {
    let mut walk = HeaderWalk {
        types: vulkan_definitions(doc, "types", "type", type_name_of),
        commands: vulkan_definitions(doc, "commands", "command", command_name_of),
        declared_types: std::collections::BTreeSet::new(),
        declared_commands: std::collections::BTreeSet::new(),
    };
    let mut blocks: Vec<_> = doc
        .descendants()
        .filter(|node| is_vulkan_block(*node))
        .collect();
    blocks.sort_by_key(|block| emission_rank(*block));

    for block in blocks {
        let mut reached = Vec::new();
        for requirement in vulkan_requirements(block) {
            // Types before commands within each `<require>`, as the generator
            // takes them, so the listed types come first in the descriptor too.
            for listed in requirement
                .children()
                .filter(|node| node.has_tag_name("type"))
                .filter_map(|node| node.attribute("name"))
            {
                walk.declare_type(listed, &mut reached);
            }
            for command in requirement
                .children()
                .filter(|node| node.has_tag_name("command"))
                .filter_map(|node| node.attribute("name"))
            {
                walk.declare_command(command, &mut reached);
            }
        }
        if block.attribute("name") == Some(extension) {
            return reached;
        }
    }
    Vec::new()
}

/// The header generator's bookkeeping: what exists, and what some block has
/// already declared.
struct HeaderWalk<'a> {
    types: std::collections::BTreeMap<&'a str, Node<'a, 'a>>,
    commands: std::collections::BTreeMap<&'a str, Node<'a, 'a>>,
    declared_types: std::collections::BTreeSet<&'a str>,
    declared_commands: std::collections::BTreeSet<&'a str>,
}

impl<'a> HeaderWalk<'a> {
    /// Declare `name` in the block being walked, with its dependencies, unless
    /// an earlier block already did. A name the registry does not define as a
    /// Vulkan type — a C scalar the headers take from elsewhere — is skipped.
    fn declare_type(&mut self, name: &'a str, reached: &mut Vec<(&'a str, Node<'a, 'a>)>) {
        let Some(&node) = self.types.get(name) else {
            return;
        };
        if !self.declared_types.insert(name) {
            return;
        }
        reached.push((name, node));
        for dependency in ["alias", "requires", "bitvalues"]
            .into_iter()
            .filter_map(|attribute| node.attribute(attribute))
        {
            self.declare_type(dependency, reached);
        }
        for nested in node
            .descendants()
            .filter(|child| *child != node && child.has_tag_name("type"))
            .filter_map(|child| child.text())
        {
            self.declare_type(nested, reached);
        }
    }

    /// Declare every type a command's prototype and parameters use, following
    /// an alias to the definition it names.
    fn declare_command(&mut self, name: &'a str, reached: &mut Vec<(&'a str, Node<'a, 'a>)>) {
        let Some(&node) = self.commands.get(name) else {
            return;
        };
        if !self.declared_commands.insert(name) {
            return;
        }
        if let Some(alias) = node.attribute("alias") {
            self.declare_command(alias, reached);
        }
        for used in node
            .descendants()
            .filter(|child| child.has_tag_name("type"))
            .filter_map(|child| child.text())
        {
            self.declare_type(used, reached);
        }
    }
}

/// Every Vulkan definition under `<section>`, by name.
///
/// A few are defined twice, once for Vulkan and once for Vulkan SC; only the
/// first Vulkan one counts.
fn vulkan_definitions<'a>(
    doc: &'a Document<'a>,
    section: &str,
    tag: &str,
    name_of: fn(Node<'a, 'a>) -> Option<&'a str>,
) -> std::collections::BTreeMap<&'a str, Node<'a, 'a>> {
    let mut found = std::collections::BTreeMap::new();
    for node in doc.descendants().filter(|node| {
        node.has_tag_name(tag)
            && node
                .parent()
                .is_some_and(|parent| parent.has_tag_name(section))
            && node.attribute("api").is_none_or(names_vulkan)
    }) {
        if let Some(name) = name_of(node) {
            found.entry(name).or_insert(node);
        }
    }
    found
}

/// A top-level `<feature>` or `<extension>` that is part of Vulkan.
///
/// `<feature>` is also the spelling of a feature *bit* inside a `<require>`, so
/// the parent decides which one a node is.
fn is_vulkan_block(node: Node<'_, '_>) -> bool {
    let parent_is = |tag| node.parent().is_some_and(|parent| parent.has_tag_name(tag));
    if node.has_tag_name("feature") {
        parent_is("registry") && node.attribute("api").is_none_or(names_vulkan)
    } else if node.has_tag_name("extension") {
        parent_is("extensions") && names_vulkan(supported_apis(node))
    } else {
        false
    }
}

/// The order the header generator emits blocks in: `sortorder`, then core
/// versions, then the Khronos extensions, then every other, each by version or
/// number (Vulkan-Docs `scripts/generator.py`, `regSortFeatures`).
///
/// Equal ranks keep document order: the sort is stable, like the generator's.
fn emission_rank(block: Node<'_, '_>) -> (i64, BlockGroup, (u32, u32), i64) {
    let sortorder = block
        .attribute("sortorder")
        .and_then(|order| order.parse().ok())
        .unwrap_or(0);
    if block.has_tag_name("feature") {
        let version = block
            .attribute("number")
            .and_then(|number| number.split_once('.'))
            .and_then(|(major, minor)| Some((major.parse().ok()?, minor.parse().ok()?)))
            .unwrap_or_default();
        return (sortorder, BlockGroup::Core, version, 0);
    }
    let khronos = block
        .attribute("name")
        .and_then(|name| name.split('_').nth(1))
        .is_some_and(|tag| {
            ["KHR", "ARB", "OES"]
                .iter()
                .any(|khronos| tag.eq_ignore_ascii_case(khronos))
        });
    let number = block
        .attribute("number")
        .and_then(|number| number.parse().ok())
        .unwrap_or(0);
    let group = if khronos {
        BlockGroup::Khronos
    } else {
        BlockGroup::Other
    };
    (sortorder, group, (0, 0), number)
}

/// The generator's second sort key, in its order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BlockGroup {
    Core,
    Khronos,
    Other,
}

/// An optional attribute as owned text, empty when absent.
///
/// Most registry attributes are optional and most of them are carried verbatim,
/// so this keeps the reader from repeating the same three calls forty times.
fn text_attribute(node: Node<'_, '_>, name: &str) -> String {
    node.attribute(name).unwrap_or_default().to_owned()
}

/// One `<enum>` entry, in whichever of the four value forms it uses.
///
/// `None` for an entry carrying no value at all: the registry also uses `<enum>`
/// to *reference* an enumerant defined elsewhere, and a reference introduces
/// nothing. The forms are mutually exclusive per `registry.rnc`, so the order
/// here only decides what a malformed document does.
fn enumerator_of(
    doc: &Document<'_>,
    node: Node<'_, '_>,
    ext_number: i64,
) -> Result<Option<VkEnumerator>> {
    let Some(name) = node.attribute("name") else {
        return Ok(None);
    };
    let value = if let Some(alias) = node.attribute("alias") {
        VkEnumValue::Alias {
            alias: alias.to_owned(),
            canonical: canonical_enum_name(doc, alias)?,
        }
    } else if let Some(offset) = node.attribute("offset") {
        VkEnumValue::Offset {
            offset: offset
                .parse()
                .with_context(|| format!("enumerator {name} has a non-numeric offset"))?,
            // `extnumber` borrows another extension's reserved block. Without
            // it the enumerator sits in the block of the extension defining it.
            ext_number: match node.attribute("extnumber") {
                Some(borrowed) => borrowed
                    .parse()
                    .with_context(|| format!("enumerator {name} has a non-numeric extnumber"))?,
                None => ext_number,
            },
            negative: node.attribute("dir") == Some("-"),
        }
    } else if let Some(bitpos) = node.attribute("bitpos") {
        VkEnumValue::Bitpos {
            bitpos: bitpos
                .parse()
                .with_context(|| format!("enumerator {name} has a non-numeric bitpos"))?,
        }
    } else if let Some(literal) = node.attribute("value") {
        VkEnumValue::Literal {
            value: literal.to_owned(),
        }
    } else {
        return Ok(None);
    };

    Ok(Some(VkEnumerator {
        name: name.to_owned(),
        extends: text_attribute(node, "extends"),
        type_name: text_attribute(node, "type"),
        api: text_attribute(node, "api"),
        protect: text_attribute(node, "protect"),
        deprecated: text_attribute(node, "deprecated"),
        value,
    }))
}

/// Keep one record per enumerator name, reporting a disagreement.
///
/// An extension may name the same enumerant in two `<require>` blocks, normally
/// under different conditions and with the same value. A differing value is the
/// case worth hearing about.
fn dedupe_enumerators(
    enumerators: Vec<VkEnumerator>,
    notes: &mut Vec<String>,
) -> Vec<VkEnumerator> {
    let mut kept: Vec<VkEnumerator> = Vec::new();
    for enumerator in enumerators {
        if let Some(existing) = kept.iter().find(|entry| entry.name == enumerator.name) {
            if existing.value != enumerator.value {
                notes.push(format!(
                    "enumerator {} is given two different values; kept the first",
                    enumerator.name
                ));
            }
            continue;
        }
        kept.push(enumerator);
    }
    kept
}

/// A struct's record.
///
/// `stype` is empty for a struct that is not chainable. Those are ordinary
/// public structs — a transform matrix, an AABB — and the extension declares
/// them like any other; only the registration through a `pNext` tag is missing.
fn struct_of(node: Node<'_, '_>, name: &str) -> Result<VkStruct> {
    let extends = node.attribute("structextends").unwrap_or_default();
    Ok(VkStruct {
        name: name.to_owned(),
        stype: stype_of(node).unwrap_or_default(),
        is_features: extends.contains("VkPhysicalDeviceFeatures2"),
        is_properties: extends.contains("VkPhysicalDeviceProperties2"),
        returned_only: node.attribute("returnedonly") == Some("true"),
        struct_extends: extends.to_owned(),
        members: members_of(node)?,
    })
}

/// A new enum type and the enumerants the registry files under it.
///
/// Those come from the document's own `<enums>` block, not from the extension's
/// `<require>`: defining an enum and extending someone else's are different acts
/// and the registry writes them in different places.
fn enum_type_of(doc: &Document<'_>, name: &str) -> Result<VkEnumType> {
    let Some(block) = doc
        .descendants()
        .find(|node| node.has_tag_name("enums") && node.attribute("name") == Some(name))
    else {
        // A declared type with no values yet. Real: a flag-bits enum is often
        // introduced empty and filled by later extensions.
        return Ok(VkEnumType {
            name: name.to_owned(),
            kind: String::new(),
            bitwidth: 32,
            enumerants: Vec::new(),
        });
    };
    let mut enumerants = Vec::new();
    for node in block.children().filter(|node| node.has_tag_name("enum")) {
        // An entry in an <enums> block spells its value outright, so it never
        // needs an extension's block base.
        if let Some(enumerant) = enumerator_of(doc, node, 0)? {
            enumerants.push(enumerant);
        }
    }
    Ok(VkEnumType {
        name: name.to_owned(),
        kind: text_attribute(block, "type"),
        bitwidth: block
            .attribute("bitwidth")
            .and_then(|width| width.parse().ok())
            .unwrap_or(32),
        enumerants,
    })
}

/// What the registry's own SPIR-V tables say this extension enables.
///
/// An entry is selected when one of its `<enable>` conditions names this
/// extension. All of that entry's conditions are then carried, not just the
/// matching one: a capability is usually reachable several ways, and which route
/// a target wants to require is the target's decision.
fn spirv_requirements(doc: &Document<'_>, extension: &str) -> Vec<VkSpirvRequirement> {
    let mut found = Vec::new();
    for node in doc
        .descendants()
        .filter(|node| node.has_tag_name("spirvextension") || node.has_tag_name("spirvcapability"))
    {
        let enables: Vec<VkSpirvEnable> = node
            .children()
            .filter(|child| child.has_tag_name("enable"))
            .filter_map(spirv_enable)
            .collect();
        let names_it = enables
            .iter()
            .any(|enable| matches!(enable, VkSpirvEnable::Extension { extension: by } if by == extension));
        if !names_it {
            continue;
        }
        found.push(VkSpirvRequirement {
            kind: if node.has_tag_name("spirvextension") {
                VkSpirvKind::Extension
            } else {
                VkSpirvKind::Capability
            },
            name: text_attribute(node, "name"),
            enables,
        });
    }
    found
}

/// One `<enable>` row, in whichever of the registry's four forms it uses.
fn spirv_enable(node: Node<'_, '_>) -> Option<VkSpirvEnable> {
    if let Some(version) = node.attribute("version") {
        return Some(VkSpirvEnable::Version {
            version: version.to_owned(),
        });
    }
    if let Some(extension) = node.attribute("extension") {
        return Some(VkSpirvEnable::Extension {
            extension: extension.to_owned(),
        });
    }
    if let (Some(struct_name), Some(feature)) =
        (node.attribute("struct"), node.attribute("feature"))
    {
        return Some(VkSpirvEnable::Feature {
            struct_name: struct_name.to_owned(),
            feature: feature.to_owned(),
            requires: text_attribute(node, "requires"),
            alias: text_attribute(node, "alias"),
        });
    }
    if let (Some(property), Some(member), Some(value)) = (
        node.attribute("property"),
        node.attribute("member"),
        node.attribute("value"),
    ) {
        return Some(VkSpirvEnable::Property {
            property: property.to_owned(),
            member: member.to_owned(),
            value: value.to_owned(),
            requires: text_attribute(node, "requires"),
        });
    }
    None
}

fn find_type<'a>(doc: &'a Document<'a>, name: &str) -> Option<Node<'a, 'a>> {
    doc.descendants().find(|node| {
        node.has_tag_name("type")
            && node
                .parent()
                .is_some_and(|parent| parent.has_tag_name("types"))
            && type_name_of(*node) == Some(name)
    })
}

/// A type's name.
///
/// The registry spells it as an attribute when it generates the declaration
/// itself, and as a `<name>` child when the entry carries C text the name is
/// embedded in — which is every handle, function pointer, bitmask and base type.
fn type_name_of<'a>(node: Node<'a, 'a>) -> Option<&'a str> {
    node.attribute("name").or_else(|| child_str(node, "name"))
}

/// A command's name: an attribute on an alias, which has no body, and the
/// prototype's `<name>` on a definition.
fn command_name_of<'a>(node: Node<'a, 'a>) -> Option<&'a str> {
    node.attribute("name").or_else(|| {
        node.children()
            .find(|child| child.has_tag_name("proto"))
            .and_then(|proto| child_str(proto, "name"))
    })
}

fn canonical_type_name(doc: &Document<'_>, name: &str) -> Result<String> {
    let mut current = name.to_owned();
    let mut seen = Vec::new();
    loop {
        if seen.contains(&current) {
            bail!("type alias cycle: {}", seen.join(" -> "));
        }
        seen.push(current.clone());
        let Some(node) = find_type(doc, &current) else {
            return Ok(current);
        };
        let Some(next) = node.attribute("alias") else {
            return Ok(current);
        };
        current = next.to_owned();
    }
}

fn canonical_enum_name(doc: &Document<'_>, name: &str) -> Result<String> {
    let mut current = name.to_owned();
    let mut seen = Vec::new();
    loop {
        if seen.contains(&current) {
            bail!("enum alias cycle: {}", seen.join(" -> "));
        }
        seen.push(current.clone());
        let next = doc
            .descendants()
            .find(|node| {
                node.has_tag_name("enum") && node.attribute("name") == Some(current.as_str())
            })
            .and_then(|node| node.attribute("alias"));
        let Some(next) = next else {
            return Ok(current);
        };
        current = next.to_owned();
    }
}

fn command_of(
    doc: &Document<'_>,
    name: &str,
    requirement: VkRequirement,
    protect: &str,
) -> Result<VkCommand> {
    let public =
        find_command(doc, name).with_context(|| format!("no command definition for {name}"))?;
    let alias_of = public.attribute("alias").map(str::to_owned);
    let canonical_name = canonical_command_name(doc, name)?;
    let canonical = find_command(doc, &canonical_name)
        .with_context(|| format!("no canonical command definition for {canonical_name}"))?;
    let proto = canonical
        .children()
        .find(|node| node.has_tag_name("proto"))
        .context("command has no prototype")?;
    let canonical_spelling = child_text(proto, "name").context("command prototype has no name")?;
    let prototype = mixed_text(proto);
    let return_type = prototype
        .strip_suffix(&canonical_spelling)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .context("command prototype has no return type")?
        .to_owned();
    let params: Vec<VkParam> = canonical
        .children()
        .filter(|node| node.has_tag_name("param"))
        .map(|param| {
            let name = child_text(param, "name").context("command parameter has no name")?;
            let declaration = mixed_text(param);
            let (type_decl, suffix) = declaration
                .rsplit_once(&name)
                .map(|(before, after)| (before.trim().to_owned(), after.trim().to_owned()))
                .context("command parameter declaration does not contain its name")?;
            Ok(VkParam {
                name,
                type_name: child_text(param, "type").context("command parameter has no type")?,
                type_decl,
                suffix,
                declaration,
                optional: text_attribute(param, "optional"),
                len: text_attribute(param, "len"),
                alt_len: text_attribute(param, "altlen"),
                extern_sync: text_attribute(param, "externsync"),
                no_auto_validity: text_attribute(param, "noautovalidity"),
                object_type: text_attribute(param, "objecttype"),
                valid_structs: text_attribute(param, "validstructs"),
                stride: text_attribute(param, "stride"),
                api: text_attribute(param, "api"),
            })
        })
        .collect::<Result<_>>()?;
    let dispatch = dispatch_of(params.first().map(|param| param.type_name.as_str()))?;
    Ok(VkCommand {
        name: name.to_owned(),
        alias_of,
        canonical_name,
        return_type,
        dispatch,
        protect: protect.to_owned(),
        // Recording conditions comes off the canonical definition: an alias has
        // no body of its own, so it inherits every constraint of what it names.
        success_codes: text_attribute(canonical, "successcodes"),
        error_codes: text_attribute(canonical, "errorcodes"),
        queues: text_attribute(canonical, "queues"),
        cmd_buffer_level: text_attribute(canonical, "cmdbufferlevel"),
        render_pass: text_attribute(canonical, "renderpass"),
        video_coding: text_attribute(canonical, "videocoding"),
        tasks: text_attribute(canonical, "tasks"),
        conditional_rendering: text_attribute(canonical, "conditionalrendering"),
        superseded_by: text_attribute(public, "supersededby"),
        params,
        requirements: vec![requirement],
        meta: Default::default(),
    })
}

/// The preprocessor symbol a platform's declarations are guarded by.
///
/// The extension names a platform and the `<platforms>` table spells the macro;
/// no `<require>` in the published registry carries a `protect` attribute, so
/// this lookup is the only route to it.
fn platform_protect(doc: &Document<'_>, platform: &str) -> Option<String> {
    doc.descendants()
        .find(|node| node.has_tag_name("platform") && node.attribute("name") == Some(platform))
        .and_then(|node| node.attribute("protect"))
        .map(str::to_owned)
}

fn find_command<'a>(doc: &'a Document<'a>, name: &str) -> Option<Node<'a, 'a>> {
    doc.descendants().find(|node| {
        if !node.has_tag_name("command") {
            return false;
        }
        if !node
            .parent()
            .is_some_and(|parent| parent.has_tag_name("commands"))
        {
            return false;
        }
        command_name_of(*node) == Some(name) && node.attribute("api").is_none_or(names_vulkan)
    })
}

fn canonical_command_name(doc: &Document<'_>, name: &str) -> Result<String> {
    let mut current = name.to_owned();
    let mut seen = Vec::new();
    loop {
        if seen.contains(&current) {
            bail!("command alias cycle: {}", seen.join(" -> "));
        }
        seen.push(current.clone());
        let node = find_command(doc, &current)
            .with_context(|| format!("no command definition for {current}"))?;
        let Some(next) = node.attribute("alias") else {
            return Ok(current);
        };
        current = next.to_owned();
    }
}

fn dispatch_of(first_type: Option<&str>) -> Result<VkDispatch> {
    Ok(match first_type {
        None => VkDispatch::Global,
        Some("VkInstance") => VkDispatch::Instance,
        Some("VkPhysicalDevice") => VkDispatch::PhysicalDevice,
        Some("VkDevice") => VkDispatch::Device,
        Some("VkQueue") => VkDispatch::Queue,
        Some("VkCommandBuffer") => VkDispatch::CommandBuffer,
        Some(other) => bail!("cannot classify command dispatch from first parameter type {other}"),
    })
}

fn mixed_text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(|child| child.is_text())
        .filter_map(|child| child.text())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `VkStructureType` value tagging a struct, read off its `sType` member.
fn stype_of<'a>(definition: Node<'a, 'a>) -> Option<String> {
    definition
        .children()
        .filter(|node| node.has_tag_name("member"))
        .find_map(|member| member.attribute("values").map(str::to_owned))
}

/// The members past the common `sType`/`pNext` pair, which every generated
/// struct spells identically and so needs no data to reproduce.
fn members_of<'a>(definition: Node<'a, 'a>) -> Result<Vec<VkMember>> {
    definition
        .children()
        .filter(|node| node.has_tag_name("member"))
        .filter_map(|member| {
            let name = child_text(member, "name")?;
            if name == "sType" || name == "pNext" {
                return None;
            }
            Some((member, name))
        })
        .map(|(member, name)| {
            let declaration = mixed_text(member);
            let (type_decl, suffix) = declaration
                .rsplit_once(&name)
                .map(|(before, after)| (before.trim().to_owned(), after.trim().to_owned()))
                .context("struct member declaration does not contain its name")?;
            Ok(VkMember {
                name,
                type_name: child_text(member, "type").context("struct member has no type")?,
                type_decl,
                suffix,
                declaration,
                optional: text_attribute(member, "optional"),
                limit_type: text_attribute(member, "limittype"),
                len: text_attribute(member, "len"),
                alt_len: text_attribute(member, "altlen"),
                no_auto_validity: text_attribute(member, "noautovalidity"),
                extern_sync: text_attribute(member, "externsync"),
                object_type: text_attribute(member, "objecttype"),
                selector: text_attribute(member, "selector"),
                selection: text_attribute(member, "selection"),
                values: text_attribute(member, "values"),
                api: text_attribute(member, "api"),
                deprecated: text_attribute(member, "deprecated"),
            })
        })
        .collect()
}

fn child_str<'a>(node: Node<'a, 'a>, tag: &str) -> Option<&'a str> {
    node.children()
        .find(|child| child.has_tag_name(tag))
        .and_then(|child| child.text())
}

fn child_text<'a>(node: Node<'a, 'a>, tag: &str) -> Option<String> {
    child_str(node, tag).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic registry that keeps the published document's structure. See
    /// the file's own header for what each block is there to exercise.
    ///
    /// One document rather than a fixture per test: most of what this reader
    /// gets wrong is an indirection between two blocks — a platform to its
    /// macro, a struct to its tag, a type to the version that introduced it —
    /// and those only exist in a document shaped like the real one.
    const REGISTRY: &str = include_str!("testdata/registry.xml");

    fn plane() -> VkPlane {
        extract(REGISTRY, "VK_TEST_widget").unwrap().0
    }

    #[test]
    fn a_platform_extension_carries_its_protect_macro() {
        // The macro guarding a platform extension's declarations is named only
        // in <platforms>, reached through the extension's `platform` attribute.
        // No <require> in the published registry carries `protect`, so reading
        // it from there yields an empty string for all 44 platform extensions
        // and their declarations are emitted unguarded.
        let (plane, _) = extract(REGISTRY, "VK_TEST_platform_widget").unwrap();
        let command = plane
            .commands
            .iter()
            .find(|command| command.name == "vkTestWidgetTEST")
            .expect("the extension requires this command");
        assert_eq!(command.protect, "VK_USE_PLATFORM_TEST");
    }

    #[test]
    fn an_extension_unsupported_on_vulkan_is_refused() {
        // `supported` lists the APIs an extension belongs to. A disabled one is
        // still fully spelled out in the document, so nothing else distinguishes
        // it from an extension worth scaffolding.
        let err = extract(REGISTRY, "VK_TEST_disabled_widget")
            .unwrap_err()
            .to_string();
        assert!(err.contains("supported"), "got: {err}");
    }

    #[test]
    fn reads_the_extension_level_registry_facts() {
        let plane = plane();
        assert_eq!(plane.name, "VK_TEST_widget");
        assert_eq!(plane.feature, "TEST_WIDGET");
        assert_eq!(plane.snake, "vk_test_widget");
        assert_eq!(plane.number, 232);
        assert_eq!(plane.spec_version, 3);
        assert_eq!(plane.author, "TEST");
        assert!(plane.depends.contains("VK_VERSION_1_1"));
        assert!(matches!(plane.extension_type, VkExtensionType::Device));
    }

    /// The offset an enumerator carries, or `None` if it is spelled some other
    /// way. Used by the sType pairing checks, which are about *which* enumerator
    /// a struct is tagged by.
    fn offset_of(plane: &VkPlane, name: &str) -> Option<i64> {
        match &plane.enumerators.iter().find(|e| e.name == name)?.value {
            VkEnumValue::Offset { offset, .. } => Some(*offset),
            _ => None,
        }
    }

    #[test]
    fn pairs_each_struct_with_its_own_enumerator_offset() {
        let plane = plane();
        let features = plane
            .structs
            .iter()
            .find(|s| s.is_features)
            .expect("a features struct");
        assert_eq!(
            features.stype,
            "VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND"
        );
        assert_eq!(offset_of(&plane, &features.stype), Some(0));

        let properties = plane
            .structs
            .iter()
            .find(|s| s.is_properties)
            .expect("a properties struct");
        // Offset 2, not 1: the pairing comes from the sType attribute, not from
        // the order the enumerators appear in.
        assert_eq!(offset_of(&plane, &properties.stype), Some(2));
    }

    #[test]
    fn every_enumerant_is_kept_whatever_it_extends() {
        // The sType entries are not privileged: an extension contributes far
        // more to VkFormat, VkResult and the flag-bits enums than to
        // VkStructureType, and all of it has to be registered somewhere.
        let plane = plane();
        let format = plane
            .enumerators
            .iter()
            .find(|e| e.name == "VK_FORMAT_WIDGET_TEST")
            .expect("the VkFormat entry");
        assert_eq!(format.extends, "VkFormat");
        assert_eq!(
            format.value,
            VkEnumValue::Offset {
                offset: 0,
                ext_number: 232,
                negative: false
            }
        );

        let bit = plane
            .enumerators
            .iter()
            .find(|e| e.name == "VK_WIDGET_USAGE_FAST_BIT_TEST")
            .expect("the flag bit");
        assert_eq!(bit.value, VkEnumValue::Bitpos { bitpos: 7 });

        let error = plane
            .enumerators
            .iter()
            .find(|e| e.name == "VK_ERROR_WIDGET_LOST_TEST")
            .expect("the error code");
        // `dir="-"` puts an error code below the block base. Dropping it would
        // place the enumerator on top of an unrelated success code.
        assert_eq!(
            error.value,
            VkEnumValue::Offset {
                offset: 1,
                ext_number: 232,
                negative: true
            }
        );
    }

    #[test]
    fn an_enumerator_borrowing_another_block_keeps_that_number() {
        let plane = plane();
        let borrowed = plane
            .enumerators
            .iter()
            .find(|e| e.name == "VK_FORMAT_BORROWED_TEST")
            .expect("the borrowing entry");
        assert_eq!(
            borrowed.value,
            VkEnumValue::Offset {
                offset: 4,
                ext_number: 99,
                negative: false
            }
        );
    }

    #[test]
    fn new_types_are_kept_under_the_registry_category_that_named_them() {
        let plane = plane();
        assert_eq!(plane.handles.len(), 1);
        assert_eq!(plane.handles[0].name, "VkWidgetSessionTEST");
        assert!(!plane.handles[0].dispatchable);
        assert_eq!(plane.handles[0].parent, "VkDevice");
        assert_eq!(
            plane.handles[0].object_type_enum,
            "VK_OBJECT_TYPE_WIDGET_SESSION_TEST"
        );

        assert_eq!(plane.unions.len(), 1);
        assert_eq!(plane.unions[0].members.len(), 2);

        assert_eq!(plane.bitmasks.len(), 1);
        assert_eq!(plane.bitmasks[0].requires, "VkWidgetUsageFlagBitsTEST");

        // A new enum type takes its values from the document's own <enums>
        // block, not from the extension's <require>.
        let kind = plane
            .enums
            .iter()
            .find(|e| e.name == "VkWidgetKindTEST")
            .expect("the new enum type");
        assert_eq!(kind.kind, "enum");
        assert_eq!(kind.enumerants.len(), 2);
        assert_eq!(
            kind.enumerants[0].value,
            VkEnumValue::Literal {
                value: "0".to_owned()
            }
        );
    }

    #[test]
    fn the_registrys_own_spirv_link_is_read_rather_than_guessed() {
        let plane = plane();
        let entry = plane
            .spirv
            .iter()
            .find(|entry| entry.name == "SPV_TEST_widget")
            .expect("the spirvextension row naming this extension");
        assert!(matches!(entry.kind, VkSpirvKind::Extension));

        let capability = plane
            .spirv
            .iter()
            .find(|entry| entry.name == "WidgetTEST")
            .expect("the spirvcapability row");
        // Every route is carried, not just the one that matched: which to
        // require is the target's call.
        assert_eq!(capability.enables.len(), 2);
        assert!(capability.enables.iter().any(|enable| matches!(
            enable,
            VkSpirvEnable::Feature { feature, .. } if feature == "widgetEnabled"
        )));
    }

    #[test]
    fn structextends_classifies_features_against_properties() {
        let plane = plane();
        let features = plane.structs.iter().find(|s| s.is_features).unwrap();
        assert!(!features.is_properties);
        let properties = plane.structs.iter().find(|s| s.is_properties).unwrap();
        assert!(!properties.is_features);
    }

    #[test]
    fn the_common_header_members_are_dropped() {
        let plane = plane();
        let features = plane.structs.iter().find(|s| s.is_features).unwrap();
        assert_eq!(features.members.len(), 2);
        assert_eq!(features.members[0].name, "widgetEnabled");
        assert_eq!(features.members[0].type_decl, "VkBool32");
        assert_eq!(features.members[1].type_decl, "const char*");
        assert_eq!(features.members[1].suffix, "[4]");
    }

    #[test]
    fn a_listed_type_that_core_declares_is_left_to_core() {
        // The extension lists `VkFormat` because its enumerators extend it, but
        // core declared it first; generating it again would redeclare it.
        let plane = plane();
        assert!(plane.enums.iter().all(|e| e.name != "VkFormat"));
        assert_eq!(plane.structs.len(), 2);
    }

    /// The struct, union and enum names an extension's descriptor declares —
    /// the categories the emission-order blocks of the fixture use.
    fn declared_names(name: &str) -> Vec<String> {
        let (plane, _) = extract(REGISTRY, name).unwrap();
        let structs = plane.structs.into_iter().map(|t| t.name);
        let unions = plane.unions.into_iter().map(|t| t.name);
        let enums = plane.enums.into_iter().map(|t| t.name);
        structs.chain(unions).chain(enums).collect()
    }

    #[test]
    fn a_khronos_extension_declares_its_type_before_a_lower_numbered_vendor_one() {
        // The generator emits every KHR extension before any vendor one, so
        // `VkComponentTypeKHR` is declared under VK_KHR_cooperative_matrix even
        // though VK_NV_cooperative_vector, numbered lower, names it too.
        assert!(declared_names("VK_KHR_test_scope").contains(&"VkWidgetScopeKHR".to_owned()));
        assert!(!declared_names("VK_NV_test_scope_user").contains(&"VkWidgetScopeKHR".to_owned()));
    }

    #[test]
    fn a_sortorder_defers_an_extension_past_the_blocks_that_use_its_types() {
        // VK_KHR_acceleration_structure is `sortorder="1"`, which is why the
        // header declares most of its types under VK_NV_ray_tracing and others.
        assert!(declared_names("VK_EXT_test_early").contains(&"VkWidgetAddressKHR".to_owned()));
        assert!(!declared_names("VK_KHR_test_deferred").contains(&"VkWidgetAddressKHR".to_owned()));
    }

    #[test]
    fn a_type_reached_only_as_a_dependency_is_declared_with_what_reached_it() {
        // No <require> lists `VkWidgetEarlyModeEXT`; the struct that uses it
        // does, and the header declares the two together.
        let names = declared_names("VK_EXT_test_early");
        assert!(
            names.contains(&"VkWidgetEarlyInfoEXT".to_owned()),
            "{names:?}"
        );
        assert!(
            names.contains(&"VkWidgetEarlyModeEXT".to_owned()),
            "{names:?}"
        );
    }

    #[test]
    fn features_carry_the_struct_that_reports_them() {
        let plane = plane();
        assert_eq!(plane.features.len(), 1);
        assert_eq!(plane.features[0].name, "widgetEnabled");
        assert_eq!(
            plane.features[0].struct_name,
            "VkPhysicalDeviceWidgetFeaturesVND"
        );
    }

    #[test]
    fn commands_keep_exact_signatures_dispatch_and_aliases() {
        let plane = plane();
        let command = plane
            .commands
            .iter()
            .find(|command| command.name == "vkWidgetTEST")
            .unwrap();
        assert!(matches!(command.dispatch, VkDispatch::Device));
        assert_eq!(command.return_type, "VkResult");
        assert_eq!(
            command.params[1].declaration,
            "const VkPhysicalDeviceWidgetFeaturesVND* pInfo"
        );
        assert_eq!(command.params[2].type_decl, "uint32_t");
        assert_eq!(command.params[2].suffix, "[4]");
        assert_eq!(command.requirements[0].depends, "VK_TEST_condition");

        let alias = plane
            .commands
            .iter()
            .find(|command| command.name == "vkWidgetAliasTEST")
            .unwrap();
        assert_eq!(alias.alias_of.as_deref(), Some("vkWidgetTEST"));
        assert_eq!(alias.canonical_name, "vkWidgetTEST");
        assert_eq!(alias.params, command.params);
    }

    #[test]
    fn type_and_enum_aliases_preserve_immediate_and_canonical_targets() {
        let plane = plane();
        assert_eq!(plane.type_aliases.len(), 1);
        assert_eq!(plane.type_aliases[0].name, "VkWidgetAliasVND");
        assert_eq!(
            plane.type_aliases[0].canonical_name,
            "VkPhysicalDeviceWidgetFeaturesVND"
        );
        // An aliasing enumerator is an ordinary enumerator whose value happens
        // to be a name, so it lives with the rest rather than in a list of its
        // own. The end of the chain is resolved because following it needs the
        // whole document.
        let alias = plane
            .enumerators
            .iter()
            .find(|e| e.name == "VK_STRUCTURE_TYPE_WIDGET_ALIAS_VND")
            .expect("the aliasing enumerator");
        assert_eq!(
            alias.value,
            VkEnumValue::Alias {
                alias: "VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND".to_owned(),
                canonical: "VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND".to_owned(),
            }
        );
    }

    #[test]
    fn an_unknown_extension_says_what_to_do_instead() {
        let err = extract(REGISTRY, "VK_TEST_absent").unwrap_err().to_string();
        assert!(err.contains("by hand"), "got: {err}");
    }

    #[test]
    fn malformed_xml_is_reported_as_such() {
        let err = extract("<registry>", "VK_TEST_x").unwrap_err().to_string();
        assert!(err.contains("well-formed"), "got: {err}");
    }

    #[test]
    fn an_extension_with_no_api_surface_is_refused() {
        let xml = r#"<registry><extensions>
            <extension name="VK_TEST_empty" number="9"><require/></extension>
        </extensions></registry>"#;
        let err = extract(xml, "VK_TEST_empty").unwrap_err().to_string();
        assert!(err.contains("no modeled API surface"), "got: {err}");
    }

    /// The tag of the features struct, spelled exactly as the fixture does, so
    /// the rewriting tests below can swap it for another value form.
    const FEATURES_TAG: &str = r#"<enum offset="0" extends="VkStructureType" name="VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND"/>"#;

    #[test]
    fn an_alias_is_preserved_instead_of_inventing_an_offset() {
        let xml = REGISTRY.replace(
            FEATURES_TAG,
            r#"<enum extends="VkStructureType" alias="VK_STRUCTURE_TYPE_PROMOTED" name="VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND"/>"#,
        );
        let (plane, notes) = extract(&xml, "VK_TEST_widget").unwrap();
        let features = plane.structs.iter().find(|s| s.is_features).unwrap();
        let tag = plane
            .enumerators
            .iter()
            .find(|e| e.name == features.stype)
            .expect("the struct's tag");
        assert_eq!(
            tag.value,
            VkEnumValue::Alias {
                alias: "VK_STRUCTURE_TYPE_PROMOTED".to_owned(),
                canonical: "VK_STRUCTURE_TYPE_PROMOTED".to_owned(),
            }
        );
        assert!(!notes.iter().any(|n| n.contains("no value")));
    }

    #[test]
    fn a_tag_with_no_value_at_all_is_reported() {
        // `<enum>` is also how the registry *references* an enumerant defined
        // elsewhere. A struct tagged by one of those cannot be registered, and
        // dropping it without a word is how a scaffold ends up half applied.
        let xml = REGISTRY.replace(
            FEATURES_TAG,
            r#"<enum extends="VkStructureType" name="VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND"/>"#,
        );
        let (plane, notes) = extract(&xml, "VK_TEST_widget").unwrap();
        assert!(plane.structs.iter().all(|s| !s.is_features));
        assert!(
            notes.iter().any(|n| n.contains("no value")),
            "got: {notes:?}"
        );
    }

    #[test]
    fn a_tag_this_extension_never_names_is_reported() {
        // The published case is a struct that arrives as a dependency: a
        // vendor extension listing its alias of an EXT struct is where the
        // header declares that struct, while the EXT extension gives its tag a
        // value. Dropping it silently would hide why it never gets registered.
        let xml = REGISTRY.replace(FEATURES_TAG, "");
        let (plane, notes) = extract(&xml, "VK_TEST_widget").unwrap();
        assert!(plane.structs.iter().all(|s| !s.is_features));
        assert!(
            notes.iter().any(|n| n.contains("no value")),
            "got: {notes:?}"
        );
    }

    #[test]
    fn a_pointer_member_keeps_its_star() {
        let xml = REGISTRY.replace(
            "<member><type>VkBool32</type>  <name>widgetEnabled</name></member>",
            "<member><type>char</type>* <name>pName</name></member>",
        );
        let (plane, _) = extract(&xml, "VK_TEST_widget").unwrap();
        let features = plane.structs.iter().find(|s| s.is_features).unwrap();
        assert_eq!(features.members[0].type_decl, "char*");
    }

    #[test]
    fn alias_cycles_are_rejected() {
        let xml = r#"<registry>
          <types>
            <type name="VkAliasA" alias="VkAliasB"/>
            <type name="VkAliasB" alias="VkAliasA"/>
          </types>
          <commands>
            <command name="vkAliasA" alias="vkAliasB"/>
            <command name="vkAliasB" alias="vkAliasA"/>
          </commands>
          <enums>
            <enum name="VK_ALIAS_A" alias="VK_ALIAS_B"/>
            <enum name="VK_ALIAS_B" alias="VK_ALIAS_A"/>
          </enums>
        </registry>"#;
        let doc = Document::parse(xml).unwrap();
        assert!(canonical_type_name(&doc, "VkAliasA").is_err());
        assert!(canonical_command_name(&doc, "vkAliasA").is_err());
        assert!(canonical_enum_name(&doc, "VK_ALIAS_A").is_err());
    }
}
