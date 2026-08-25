//! Select one extension out of the Vulkan registry, `vk.xml`.
//!
//! The registry is machine-readable and complete, so nothing here is heuristic.
//! One chain is worth spelling out because it is what avoids guesswork: an
//! extension's `<require>` block names its structs and, separately, the
//! `VkStructureType` enumerators it defines —
//!
//! ```xml
//! <type name="VkPhysicalDeviceWidgetFeaturesVND" />
//! <enum offset="0" extends="VkStructureType"
//!       name="VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND" />
//! ```
//!
//! and the two are linked *inside* the struct, whose `sType` member carries
//! `values="VK_STRUCTURE_TYPE_…"`. Following that attribute pairs a struct with
//! its enumerator exactly, where deriving one name from the other by
//! case-mangling would be a guess that fails on the irregular ones.
//!
//! The `offset` and extension number together determine an enumerator value, so
//! pairing a struct with the wrong offset silently produces a collision.

use anyhow::{bail, Context, Result};
use roxmltree::{Document, Node};

use crate::model::{
    VkAlias, VkCommand, VkDispatch, VkExtensionType, VkFeature, VkMember, VkParam, VkPlane,
    VkRequirement, VkStruct,
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
    plane.depends = extension
        .attribute("depends")
        .unwrap_or_default()
        .to_owned();

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

    let requirements: Vec<_> = extension
        .children()
        .filter(|node| node.has_tag_name("require"))
        .filter(|requirement| {
            requirement
                .attribute("api")
                .is_none_or(|api| api.split(',').any(|entry| entry == "vulkan"))
        })
        .collect();

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

    for requirement in &requirements {
        let condition = VkRequirement {
            depends: requirement
                .attribute("depends")
                .unwrap_or_default()
                .to_owned(),
            protect: requirement
                .attribute("protect")
                .unwrap_or_default()
                .to_owned(),
            api: requirement.attribute("api").unwrap_or_default().to_owned(),
        };
        for command_name in requirement
            .children()
            .filter(|node| node.has_tag_name("command"))
            .filter_map(|node| node.attribute("name"))
        {
            let command = command_of(&doc, command_name, condition.clone())?;
            match plane
                .commands
                .iter_mut()
                .find(|existing| existing.name == command.name)
            {
                Some(existing) => {
                    if existing.protect != command.protect {
                        bail!(
                            "command {} has incompatible protection macros '{}' and '{}'",
                            command.name,
                            existing.protect,
                            command.protect
                        );
                    }
                    existing.requirements.extend(command.requirements);
                }
                None => plane.commands.push(command),
            }
        }
    }

    // Enumerator name -> offset, for the structs below. Only enumerators with an
    // `offset` are computable; an `alias` of a promoted core enumerator has a
    // fixed spelling instead and needs no arithmetic.
    let offsets: Vec<(String, i64)> = requirements
        .iter()
        .flat_map(|requirement| requirement.descendants())
        .filter(|node| {
            node.has_tag_name("enum") && node.attribute("extends") == Some("VkStructureType")
        })
        .filter_map(|node| {
            Some((
                node.attribute("name")?.to_owned(),
                node.attribute("offset")?.parse().ok()?,
            ))
        })
        .collect();

    for referenced in requirements
        .iter()
        .flat_map(|requirement| requirement.descendants())
        .filter(|node| node.has_tag_name("type"))
        .filter_map(|node| node.attribute("name"))
    {
        let Some(type_node) = find_type(&doc, referenced) else {
            continue;
        };
        if let Some(alias_of) = type_node.attribute("alias") {
            plane.type_aliases.push(VkAlias {
                name: referenced.to_owned(),
                alias_of: alias_of.to_owned(),
                canonical_name: canonical_type_name(&doc, referenced)?,
            });
            continue;
        }
        if type_node.attribute("category") != Some("struct") {
            // Extensions reference handle and enum types too; only structs have
            // a body to generate.
            continue;
        }
        let Some(stype) = stype_of(type_node) else {
            notes.push(format!("{referenced}: no sType member, skipped"));
            continue;
        };
        let stype_entry = requirements
            .iter()
            .flat_map(|requirement| requirement.descendants())
            .find(|node| {
                node.has_tag_name("enum") && node.attribute("name") == Some(stype.as_str())
            });
        let Some(stype_entry) = stype_entry else {
            // The referenced struct already exists outside this extension.
            continue;
        };
        let offset = offsets.iter().find(|(n, _)| *n == stype).map(|(_, o)| *o);
        let stype_alias_of = stype_entry.attribute("alias").map(str::to_owned);
        if offset.is_none() && stype_alias_of.is_none() {
            notes.push(format!(
                "{stype}: no offset in this extension's require block, so its value cannot be \
                 computed"
            ));
            continue;
        }
        let extends = type_node.attribute("structextends").unwrap_or_default();
        plane.structs.push(VkStruct {
            name: referenced.to_owned(),
            stype,
            stype_offset: offset,
            stype_alias_of,
            is_features: extends.contains("VkPhysicalDeviceFeatures2"),
            is_properties: extends.contains("VkPhysicalDeviceProperties2"),
            members: members_of(type_node)?,
        });
    }

    for node in requirements
        .iter()
        .flat_map(|requirement| requirement.descendants())
        .filter(|node| node.has_tag_name("enum"))
    {
        let (Some(name), Some(alias_of)) = (node.attribute("name"), node.attribute("alias")) else {
            continue;
        };
        plane.enum_aliases.push(VkAlias {
            name: name.to_owned(),
            alias_of: alias_of.to_owned(),
            canonical_name: canonical_enum_name(&doc, alias_of)?,
        });
    }
    plane.type_aliases = dedupe_aliases(plane.type_aliases, "type", &mut notes);
    plane.enum_aliases = dedupe_aliases(plane.enum_aliases, "enumerator", &mut notes);

    if plane.structs.is_empty()
        && plane.features.is_empty()
        && plane.commands.is_empty()
        && plane.type_aliases.is_empty()
        && plane.enum_aliases.is_empty()
    {
        bail!("{name} adds no modeled API surface; there is nothing to scaffold");
    }
    Ok((plane, notes))
}

fn dedupe_aliases(aliases: Vec<VkAlias>, what: &str, notes: &mut Vec<String>) -> Vec<VkAlias> {
    let mut kept: Vec<VkAlias> = Vec::new();
    for alias in aliases {
        if let Some(existing) = kept.iter().find(|entry| entry.name == alias.name) {
            if existing.alias_of != alias.alias_of {
                notes.push(format!(
                    "{what} alias {} names both {} and {}; kept the first",
                    alias.name, existing.alias_of, alias.alias_of
                ));
            }
            continue;
        }
        kept.push(alias);
    }
    kept
}

fn find_type<'a>(doc: &'a Document<'a>, name: &str) -> Option<Node<'a, 'a>> {
    doc.descendants().find(|node| {
        node.has_tag_name("type")
            && node
                .parent()
                .is_some_and(|parent| parent.has_tag_name("types"))
            && node.attribute("name") == Some(name)
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

fn command_of(doc: &Document<'_>, name: &str, requirement: VkRequirement) -> Result<VkCommand> {
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
        protect: requirement.protect.clone(),
        params,
        requirements: vec![requirement],
        meta: Default::default(),
    })
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
        let node_name = node.attribute("name").map(str::to_owned).or_else(|| {
            node.children()
                .find(|child| child.has_tag_name("proto"))
                .and_then(|proto| child_text(proto, "name"))
        });
        node_name.as_deref() == Some(name)
            && node
                .attribute("api")
                .is_none_or(|api| api.split(',').any(|entry| entry == "vulkan"))
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
            })
        })
        .collect()
}

fn child_text<'a>(node: Node<'a, 'a>, tag: &str) -> Option<String> {
    node.children()
        .find(|child| child.has_tag_name(tag))
        .and_then(|child| child.text())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Includes the public registry's sType/offset link and pointer spelling.
    const XML: &str = r#"<registry>
      <types>
        <type category="struct" name="VkPhysicalDeviceWidgetFeaturesVND"
              structextends="VkPhysicalDeviceFeatures2,VkDeviceCreateInfo">
          <member values="VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND"><type>VkStructureType</type> <name>sType</name></member>
          <member optional="true"><type>void</type>*  <name>pNext</name></member>
          <member><type>VkBool32</type>  <name>widgetEnabled</name></member>
          <member>const <type>char</type>* <name>names</name>[4]</member>
        </type>
        <type category="struct" name="VkPhysicalDeviceWidgetPropertiesVND"
              structextends="VkPhysicalDeviceProperties2">
          <member values="VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_PROPERTIES_VND"><type>VkStructureType</type> <name>sType</name></member>
          <member optional="true"><type>void</type>*  <name>pNext</name></member>
          <member limittype="max"><type>uint64_t</type>  <name>maxThing</name></member>
        </type>
        <type category="enum" name="VkSomeEnum"/>
        <type category="struct" name="VkWidgetAliasVND"
              alias="VkPhysicalDeviceWidgetFeaturesVND"/>
      </types>
      <commands>
        <command>
          <proto><type>VkResult</type> <name>vkWidgetTEST</name></proto>
          <param><type>VkDevice</type> <name>device</name></param>
          <param>const <type>VkPhysicalDeviceWidgetFeaturesVND</type>* <name>pInfo</name></param>
          <param><type>uint32_t</type> <name>values</name>[4]</param>
        </command>
        <command name="vkWidgetAliasTEST" alias="vkWidgetTEST"/>
      </commands>
      <extensions>
        <extension name="VK_TEST_widget" number="232" type="device" author="TEST"
                   depends="VK_TEST_prerequisite,VK_VERSION_1_1" supported="vulkan">
          <require depends="VK_TEST_condition" protect="VK_TEST_PLATFORM">
            <enum value="3" name="VK_TEST_WIDGET_SPEC_VERSION" />
            <enum value="&quot;VK_TEST_widget&quot;" name="VK_TEST_WIDGET_EXTENSION_NAME" />
            <type name="VkPhysicalDeviceWidgetFeaturesVND" />
            <type name="VkPhysicalDeviceWidgetPropertiesVND" />
            <type name="VkSomeEnum" />
            <type name="VkWidgetAliasVND" />
            <enum offset="0" extends="VkStructureType" name="VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND" />
            <enum offset="2" extends="VkStructureType" name="VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_PROPERTIES_VND" />
            <enum extends="VkStructureType" name="VK_STRUCTURE_TYPE_WIDGET_ALIAS_VND"
                  alias="VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND" />
            <command name="vkWidgetTEST" />
            <command name="vkWidgetAliasTEST" />
            <feature name="widgetEnabled" struct="VkPhysicalDeviceWidgetFeaturesVND" />
          </require>
        </extension>
      </extensions>
    </registry>"#;

    fn plane() -> VkPlane {
        extract(XML, "VK_TEST_widget").unwrap().0
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
        assert_eq!(features.stype_offset, Some(0));

        let properties = plane
            .structs
            .iter()
            .find(|s| s.is_properties)
            .expect("a properties struct");
        // Offset 2, not 1: the pairing comes from the sType attribute, not from
        // the order the enumerators appear in.
        assert_eq!(properties.stype_offset, Some(2));
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
    fn a_referenced_non_struct_type_is_skipped() {
        // `VkSomeEnum` is referenced but has no body to generate.
        assert_eq!(plane().structs.len(), 2);
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
        assert_eq!(command.protect, "VK_TEST_PLATFORM");

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
        assert_eq!(plane.enum_aliases.len(), 1);
        assert_eq!(
            plane.enum_aliases[0].alias_of,
            "VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND"
        );
    }

    #[test]
    fn an_unknown_extension_says_what_to_do_instead() {
        let err = extract(XML, "VK_TEST_absent").unwrap_err().to_string();
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

    #[test]
    fn an_alias_is_preserved_instead_of_inventing_an_offset() {
        let xml = XML.replace(
            r#"<enum offset="0" extends="VkStructureType" name="VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND" />"#,
            r#"<enum extends="VkStructureType" alias="VK_STRUCTURE_TYPE_PROMOTED" name="VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND" />"#,
        );
        let (plane, notes) = extract(&xml, "VK_TEST_widget").unwrap();
        let features = plane.structs.iter().find(|s| s.is_features).unwrap();
        assert_eq!(features.stype_offset, None);
        assert_eq!(
            features.stype_alias_of.as_deref(),
            Some("VK_STRUCTURE_TYPE_PROMOTED")
        );
        assert!(!notes.iter().any(|n| n.contains("no offset")));
    }

    #[test]
    fn a_missing_offset_and_alias_is_reported() {
        let xml = XML.replace(
            r#"<enum offset="0" extends="VkStructureType" name="VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND" />"#,
            r#"<enum extends="VkStructureType" name="VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_WIDGET_FEATURES_VND" />"#,
        );
        let (plane, notes) = extract(&xml, "VK_TEST_widget").unwrap();
        assert!(plane.structs.iter().all(|s| !s.is_features));
        assert!(notes.iter().any(|n| n.contains("no offset")));
    }

    #[test]
    fn a_pointer_member_keeps_its_star() {
        let xml = XML.replace(
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
