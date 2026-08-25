//! Per-run, target-specific metadata.
//!
//! Specification facts remain in the descriptor. Values that describe how one
//! target names or routes an opcode or command belong in a sidecar instead. The
//! sidecar is applied only while rendering its named target and never written
//! back into the descriptor.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::model::Extension;

type Metadata = BTreeMap<String, toml::Value>;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overlay {
    #[serde(default)]
    target: BTreeMap<String, TargetOverlay>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetOverlay {
    #[serde(default)]
    spv: SpvOverlay,
    #[serde(default)]
    vk: VkOverlay,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpvOverlay {
    #[serde(default)]
    opcode: BTreeMap<String, Metadata>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct VkOverlay {
    #[serde(default)]
    command: BTreeMap<String, Metadata>,
}

impl Overlay {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read overlay {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("in overlay {}", path.display()))
    }

    pub fn apply(&self, target_name: &str, extension: &Extension) -> Result<Extension> {
        let Some(target) = self.target.get(target_name) else {
            return Ok(extension.clone());
        };
        let mut extension = extension.clone();
        let mut matched_spv = BTreeSet::new();
        let mut matched_vk = BTreeSet::new();

        if let Some(spv) = extension.spv.as_mut() {
            for opcode in spv.types.iter_mut().chain(&mut spv.operations) {
                let keys: Vec<&String> = target
                    .spv
                    .opcode
                    .keys()
                    .filter(|key| *key == &opcode.name || opcode.aliases.contains(*key))
                    .collect();
                if keys.len() > 1 {
                    bail!(
                        "target '{target_name}' overlay names more than one spelling of opcode {}: {}",
                        opcode.name,
                        keys.into_iter()
                            .map(String::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                if let Some(key) = keys.first() {
                    let meta = &target.spv.opcode[*key];
                    if let Some(emit_name) = meta.get("emit_name") {
                        let emit_name = emit_name.as_str().with_context(|| {
                            format!("opcode {} overlay emit_name is not a string", opcode.name)
                        })?;
                        if emit_name != opcode.name
                            && !opcode.aliases.iter().any(|alias| alias == emit_name)
                        {
                            bail!(
                                "opcode {} overlay emit_name '{}' is neither canonical nor an alias",
                                opcode.name,
                                emit_name
                            );
                        }
                    }
                    opcode.meta.extend(meta.clone());
                    matched_spv.insert((*key).clone());
                }
            }
        }

        if let Some(vk) = extension.vk.as_mut() {
            for command in &mut vk.commands {
                if let Some(meta) = target.vk.command.get(&command.name) {
                    command.meta.extend(meta.clone());
                    matched_vk.insert(command.name.clone());
                }
            }
        }

        let unknown_spv: Vec<&str> = target
            .spv
            .opcode
            .keys()
            .filter(|key| !matched_spv.contains(*key))
            .map(String::as_str)
            .collect();
        let unknown_vk: Vec<&str> = target
            .vk
            .command
            .keys()
            .filter(|key| !matched_vk.contains(*key))
            .map(String::as_str)
            .collect();
        if !unknown_spv.is_empty() || !unknown_vk.is_empty() {
            let spv = if unknown_spv.is_empty() {
                String::new()
            } else {
                format!("; SPIR-V: {}", unknown_spv.join(", "))
            };
            let vk = if unknown_vk.is_empty() {
                String::new()
            } else {
                format!("; Vulkan: {}", unknown_vk.join(", "))
            };
            bail!("target '{target_name}' overlay names unknown entries{spv}{vk}");
        }
        Ok(extension)
    }

    pub fn validate_targets<'a>(&self, known: impl IntoIterator<Item = &'a str>) -> Result<()> {
        let known: BTreeSet<&str> = known.into_iter().collect();
        let unknown: Vec<&str> = self
            .target
            .keys()
            .map(String::as_str)
            .filter(|name| !known.contains(name))
            .collect();
        if !unknown.is_empty() {
            bail!("overlay names unknown targets: {}", unknown.join(", "));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SpvOpcode, SpvPlane, VkCommand, VkDispatch, VkExtensionType, VkPlane};

    fn extension() -> Extension {
        Extension {
            spv: Some(SpvPlane {
                operations: vec![SpvOpcode {
                    name: "OpWidgetTEST".into(),
                    aliases: vec!["OpWidgetAliasTEST".into()],
                    value: 1,
                    class: "Arithmetic".into(),
                    operands: Vec::new(),
                    capabilities: Vec::new(),
                    encoding: crate::model::SpvEncoding::default(),
                    meta: Default::default(),
                }],
                ..SpvPlane::new("SPV_TEST_widget")
            }),
            vk: Some(VkPlane {
                name: "VK_TEST_widget".into(),
                feature: "TEST_WIDGET".into(),
                snake: "vk_test_widget".into(),
                extension_type: VkExtensionType::Device,
                commands: vec![VkCommand {
                    name: "vkWidgetTEST".into(),
                    alias_of: None,
                    canonical_name: "vkWidgetTEST".into(),
                    return_type: "void".into(),
                    dispatch: VkDispatch::Device,
                    protect: String::new(),
                    params: Vec::new(),
                    requirements: Vec::new(),
                    meta: Default::default(),
                }],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn metadata_is_merged_by_target_and_alias_spelling() {
        let overlay: Overlay = toml::from_str(
            r#"
[target.alpha.spv.opcode.OpWidgetAliasTEST]
emit_name = "OpWidgetAliasTEST"

[target.alpha.vk.command.vkWidgetTEST]
owner = "device"
"#,
        )
        .unwrap();
        let applied = overlay.apply("alpha", &extension()).unwrap();
        assert_eq!(
            applied.spv.unwrap().operations[0].meta["emit_name"].as_str(),
            Some("OpWidgetAliasTEST")
        );
        assert_eq!(
            applied.vk.unwrap().commands[0].meta["owner"].as_str(),
            Some("device")
        );
    }

    #[test]
    fn an_unknown_entry_is_rejected_instead_of_ignored() {
        let overlay: Overlay = toml::from_str(
            r#"
[target.alpha.spv.opcode.OpMissingTEST]
emit_name = "OpMissingTEST"
"#,
        )
        .unwrap();
        assert!(overlay.apply("alpha", &extension()).is_err());
    }

    #[test]
    fn emit_name_must_belong_to_the_alias_family() {
        let overlay: Overlay = toml::from_str(
            r#"
[target.alpha.spv.opcode.OpWidgetTEST]
emit_name = "OpOtherTEST"
"#,
        )
        .unwrap();
        assert!(overlay.apply("alpha", &extension()).is_err());
    }

    #[test]
    fn another_targets_metadata_is_not_applied() {
        let overlay: Overlay = toml::from_str(
            r#"
[target.beta.spv.opcode.OpMissingTEST]
emit_name = "OpMissingTEST"
"#,
        )
        .unwrap();
        assert!(overlay.apply("alpha", &extension()).is_ok());
    }

    #[test]
    fn unknown_target_names_are_rejected_when_catalogues_are_known() {
        let overlay: Overlay = toml::from_str(
            r#"
[target.typo.spv.opcode.OpWidgetTEST]
emit_name = "OpWidgetTEST"
"#,
        )
        .unwrap();
        assert!(overlay.validate_targets(["alpha"]).is_err());
    }
}
