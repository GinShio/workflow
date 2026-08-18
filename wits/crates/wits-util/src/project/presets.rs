//! Preset selection, merging, and recursive application (L2 of the pipeline).
//!
//! Three questions live here, and only here: *which* presets apply and in what
//! order ([`applied_presets`] — defaults, `applies_when` matches, then CLI
//! `--preset`), how a same-named preset *merges* across org → project → repo
//! ([`effective_preset`]), and how one preset's `extends` chain *folds* into the
//! accumulating config ([`resolve_preset_into`]). The actual env/def/args folding
//! is delegated to [`super::context`], so this module stays about preset policy.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};

use super::context::{apply_def_map, apply_env_map, resolve_replace, Ctx};
use super::model::{LogicalConfig, Profile, RawPreset, Toolchain};
use super::workspace::{ProjectData, Workspace};

/// The ordered, de-duplicated list of presets to apply: `default_presets`, then
/// `applies_when` matches, then CLI `--preset`; last occurrence wins position.
pub(crate) fn applied_presets(
    ws: &Workspace,
    project: &ProjectData,
    focus: &str,
    profile: &Profile,
    toolchain: &Option<Toolchain>,
    build_type: &str,
    generator: &Option<String>,
) -> Vec<String> {
    let mut ordered: Vec<String> = project.project.default_presets.clone();

    // Auto-applied: any candidate whose merged applies_when matches.
    let axes = MatchAxes {
        build_type,
        toolchain: toolchain.as_ref().map(|t| t.name.as_str()),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        generator: generator.as_deref(),
    };
    for name in candidate_preset_names(ws, project, focus) {
        if let Some(p) = effective_preset(ws, project, focus, &name) {
            if let Some(cond) = &p.applies_when {
                if axes.matches(cond) {
                    ordered.push(name);
                }
            }
        }
    }

    ordered.extend(profile.presets.iter().cloned());

    // De-duplicate keeping the last position.
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for name in ordered.into_iter().rev() {
        if seen.insert(name.clone()) {
            out.push(name);
        }
    }
    out.reverse();
    out
}

fn candidate_preset_names(ws: &Workspace, project: &ProjectData, focus: &str) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    if let Some(org) = &project.org {
        if let Some(m) = ws.org_presets(org) {
            names.extend(m.keys().cloned());
        }
    }
    names.extend(project.project.presets.keys().cloned());
    if let Some(repo) = project.repos.get(focus) {
        names.extend(repo.presets.keys().cloned());
    }
    names.into_iter().collect()
}

/// Merge the same-named preset across org → project → repo (maps: nearest wins;
/// lists/extends/applies_when: nearest non-empty wins). A qualified `org/preset`
/// reference reaches one org's presets directly, without merging.
fn effective_preset(
    ws: &Workspace,
    project: &ProjectData,
    focus: &str,
    name: &str,
) -> Option<RawPreset> {
    if let Some((org, base)) = name.split_once('/') {
        return ws.org_presets(org).and_then(|m| m.get(base)).cloned();
    }
    let mut layers: Vec<&RawPreset> = Vec::new();
    if let Some(org) = &project.org {
        if let Some(p) = ws.org_presets(org).and_then(|m| m.get(name)) {
            layers.push(p);
        }
    }
    if let Some(p) = project.project.presets.get(name) {
        layers.push(p);
    }
    if let Some(p) = project.repos.get(focus).and_then(|r| r.presets.get(name)) {
        layers.push(p);
    }
    if layers.is_empty() {
        return None;
    }
    let mut merged = RawPreset::default();
    for layer in layers {
        for (k, v) in &layer.environment {
            merged.environment.insert(k.clone(), v.clone());
        }
        for (k, v) in &layer.definitions {
            merged.definitions.insert(k.clone(), v.clone());
        }
        if !layer.extends.0.is_empty() {
            merged.extends = layer.extends.clone();
        }
        if layer.applies_when.is_some() {
            merged.applies_when = layer.applies_when.clone();
        }
        if !layer.extra_config_args.is_empty() {
            merged.extra_config_args = layer.extra_config_args.clone();
        }
        if !layer.extra_build_args.is_empty() {
            merged.extra_build_args = layer.extra_build_args.clone();
        }
        if !layer.extra_install_args.is_empty() {
            merged.extra_install_args = layer.extra_install_args.clone();
        }
    }
    Some(merged)
}

pub(crate) fn resolve_preset_into(
    ctx: &mut Ctx,
    logical: &mut LogicalConfig,
    ws: &Workspace,
    project: &ProjectData,
    focus: &str,
    name: &str,
    seen: &mut Vec<String>,
) -> Result<()> {
    if seen.iter().any(|n| n == name) {
        seen.push(name.to_owned());
        bail!("circular preset inheritance: {}", seen.join(" -> "));
    }
    let preset = effective_preset(ws, project, focus, name)
        .with_context(|| format!("unknown preset '{name}'"))?;
    seen.push(name.to_owned());
    for parent in &preset.extends.0 {
        resolve_preset_into(ctx, logical, ws, project, focus, parent, seen)?;
    }
    seen.pop();

    apply_env_map(
        ctx,
        logical,
        &format!("preset.{name}.environment"),
        &preset.environment,
    )?;
    apply_def_map(
        ctx,
        logical,
        &format!("preset.{name}.definitions"),
        &preset.definitions,
    )?;
    // Preset lists replace what earlier layers set (they are the nearest-level
    // contribution for this preset); different presets still accumulate in order.
    resolve_replace(
        ctx,
        &preset.extra_config_args,
        &mut logical.extra_config_args,
    )?;
    resolve_replace(ctx, &preset.extra_build_args, &mut logical.extra_build_args)?;
    resolve_replace(
        ctx,
        &preset.extra_install_args,
        &mut logical.extra_install_args,
    )?;
    Ok(())
}

struct MatchAxes<'a> {
    build_type: &'a str,
    toolchain: Option<&'a str>,
    os: &'a str,
    arch: &'a str,
    generator: Option<&'a str>,
}

impl MatchAxes<'_> {
    fn matches(&self, cond: &BTreeMap<String, toml::Value>) -> bool {
        cond.iter().all(|(key, want)| {
            let actual = match key.as_str() {
                "build_type" => Some(self.build_type),
                "toolchain" => self.toolchain,
                "os" => Some(self.os),
                "arch" => Some(self.arch),
                "generator" => self.generator,
                _ => return false, // unknown match key never matches
            };
            let Some(actual) = actual else { return false };
            match want {
                toml::Value::String(s) => s == actual,
                toml::Value::Array(items) => items
                    .iter()
                    .any(|i| i.as_str().is_some_and(|s| s == actual)),
                _ => false,
            }
        })
    }
}
