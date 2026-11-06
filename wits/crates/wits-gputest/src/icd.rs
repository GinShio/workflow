//! The Vulkan ICD set: what the composed environment exposes, which of it the
//! loader selects, and the content that identifies it.
//!
//! `VK_DRIVER_FILES` wins over `VK_ICD_FILENAMES`; a directory entry expands to
//! its `*.json` manifests. A manifest's `library_path` is relative to the
//! manifest's directory; a bare filename is refused because the loader would
//! search system paths and identity could not name what loaded.
//!
//! `VK_LOADER_DRIVERS_SELECT` follows the loader's own filter rule (Vulkan-Loader
//! `loader/loader_environment.c`: `parse_generic_filter_environment_var`,
//! `determine_filter_type`, `check_name_matches_filter_environment_var`).
//! Tokens are comma-separated and not trimmed; each is compared,
//! case-insensitively, with the manifest's file name as a prefix (`x*`), a
//! suffix (`*x`), a substring (`*x*`) or the whole name (`x`), and `*`, `**`
//! and `~all~` match every name. A `*` anywhere else, `?` and `[` are literal,
//! so a selector here selects exactly what the loader would.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use wits_actions::Env;

use crate::content;
use crate::util;

/// The manifests the environment names, before selection.
fn manifests(env: &Env) -> Result<Vec<PathBuf>> {
    let variable = if env.contains_key(OsStr::new("VK_DRIVER_FILES")) {
        "VK_DRIVER_FILES"
    } else {
        "VK_ICD_FILENAMES"
    };
    let mut out = Vec::new();
    for path in util::env_paths(env, variable) {
        if path.is_dir() {
            for entry in std::fs::read_dir(&path)
                .with_context(|| format!("reading ICD directory {}", path.display()))?
            {
                let manifest = entry
                    .with_context(|| format!("reading ICD directory {}", path.display()))?
                    .path();
                if manifest.extension().is_some_and(|ext| ext == "json") {
                    out.push(manifest);
                }
            }
        } else {
            out.push(path);
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// The manifests the loader selects. A missing or empty selector selects all.
fn select(all: &[PathBuf], selector: Option<&str>) -> Vec<PathBuf> {
    let tokens: Vec<String> = selector
        .unwrap_or("")
        .split(',')
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if tokens.is_empty() {
        return all.to_vec();
    }
    all.iter()
        .filter(|manifest| {
            let name = manifest
                .file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            tokens.iter().any(|token| filter_matches(token, &name))
        })
        .cloned()
        .collect()
}

/// One lower-cased loader filter token against a lower-cased file name.
fn filter_matches(token: &str, name: &str) -> bool {
    if token.starts_with('~') || token == "*" || token == "**" {
        return matches!(token, "~all~" | "*" | "**");
    }
    match (token.strip_prefix('*'), token.strip_suffix('*')) {
        (Some(_), Some(_)) => name.contains(&token[1..token.len() - 1]),
        (Some(suffix), None) => name.ends_with(suffix),
        (None, Some(prefix)) => name.starts_with(prefix),
        (None, None) => name == token,
    }
}

/// The deqp driver guard: exactly one selected ICD, and it is installed.
pub fn guard(env: &Env) -> Result<()> {
    let all = manifests(env)?;
    if all.is_empty() {
        bail!(
            "driver exposes no ICD: the composed environment names no \
             VK_DRIVER_FILES or VK_ICD_FILENAMES manifest"
        );
    }
    let selector = util::env_text(env, "VK_LOADER_DRIVERS_SELECT")?;
    let selected = select(&all, selector);
    let [manifest] = selected.as_slice() else {
        let names = all
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        match selector.filter(|selector| !selector.is_empty()) {
            Some(selector) => bail!(
                "VK_LOADER_DRIVERS_SELECT={selector:?} matched {} of the ICDs ({names}); \
                 a deqp case loads exactly one",
                selected.len()
            ),
            None => bail!(
                "the composed environment exposes {} ICDs ({names}); \
                 set the driver's select to name exactly one",
                all.len()
            ),
        }
    };
    if !manifest.is_file() {
        bail!(
            "selected ICD manifest {} is missing or not a regular file; \
             install the driver first (`wits build --install`)",
            manifest.display()
        );
    }
    Ok(())
}

/// The `driver.icd` component: the selected manifests and the libraries they
/// name, as a content set. It does not require exactly one manifest; the kind's
/// guard does.
pub fn component(env: &Env) -> Result<String> {
    let all = manifests(env)?;
    let selector = util::env_text(env, "VK_LOADER_DRIVERS_SELECT")?;
    let mut entries = Vec::new();
    for manifest in select(&all, selector) {
        entries.push((
            format!("manifest:{}", manifest.to_string_lossy()),
            content::hash_file(&manifest)?,
        ));
        let library = library_path(&manifest)?;
        let hash = content::hash_file(&library).with_context(|| {
            format!(
                "hashing the library {} named by {}",
                library.display(),
                manifest.display()
            )
        })?;
        entries.push((format!("library:{}", library.to_string_lossy()), hash));
    }
    content::hash_set(&entries)
}

/// The library a manifest names, resolved against the manifest's directory.
fn library_path(manifest: &Path) -> Result<PathBuf> {
    let bytes = std::fs::read(manifest)
        .with_context(|| format!("reading ICD manifest {}", manifest.display()))?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing ICD manifest {}", manifest.display()))?;
    let Some(library) = json
        .get("ICD")
        .and_then(|icd| icd.get("library_path"))
        .and_then(|value| value.as_str())
    else {
        bail!(
            "ICD manifest {} has no ICD.library_path; the loader cannot use it",
            manifest.display()
        );
    };
    if !library.contains('/') {
        bail!(
            "ICD manifest {} names a bare library_path {library:?}; the loader would search \
             system paths, so the identity cannot name what loaded",
            manifest.display()
        );
    }
    let parent = manifest
        .parent()
        .with_context(|| format!("{} has no parent directory", manifest.display()))?;
    Ok(parent.join(library))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn manifest(dir: &Path, name: &str, library: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(
            &path,
            format!(r#"{{"file_format_version":"1.0.0","ICD":{{"library_path":{library:?}}}}}"#),
        )
        .unwrap();
        path
    }

    fn names(selected: &[PathBuf]) -> Vec<String> {
        selected
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_directory_expands_to_json_manifests_only() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), "radeon.json", "./libradeon.so");
        manifest(dir.path(), "lvp.json", "./liblvp.so");
        fs::write(dir.path().join("README"), "").unwrap();
        let mut env = Env::new();
        env.insert("VK_DRIVER_FILES".into(), dir.path().as_os_str().to_owned());
        assert_eq!(
            names(&manifests(&env).unwrap()),
            ["lvp.json", "radeon.json"]
        );
    }

    #[test]
    fn selection_follows_the_loader_filter_forms() {
        let all = [
            PathBuf::from("/a/radeon_icd.x86_64.json"),
            PathBuf::from("/a/lvp_icd.x86_64.json"),
        ];
        let pick = |selector| names(&select(&all, Some(selector)));
        assert_eq!(pick("*RADEON*"), ["radeon_icd.x86_64.json"]);
        assert_eq!(pick("lvp*"), ["lvp_icd.x86_64.json"]);
        assert_eq!(pick("*.x86_64.json").len(), 2);
        assert_eq!(pick("radeon_icd.x86_64.json"), ["radeon_icd.x86_64.json"]);
        assert_eq!(pick("*lvp*,*radeon*").len(), 2);
        assert_eq!(pick("*").len(), 2);
        assert_eq!(pick("~all~").len(), 2);
        assert_eq!(names(&select(&all, Some(""))).len(), 2);
        assert_eq!(names(&select(&all, None)).len(), 2);

        // Neither a glob in the middle, a class, `?`, nor a padded token is
        // anything the loader understands: it matches no file name.
        for selector in [
            "radeon*json",
            "[rl]vp*",
            "radeon_icd.??????.json",
            "*lvp*, *radeon*",
        ] {
            assert!(
                !pick(selector).contains(&"radeon_icd.x86_64.json".to_owned()),
                "{selector}"
            );
        }
    }

    #[test]
    fn the_guard_requires_exactly_one_installed_selected_manifest() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), "radeon.json", "./libradeon.so");
        manifest(dir.path(), "lvp.json", "./liblvp.so");
        let mut env = Env::new();
        env.insert("VK_DRIVER_FILES".into(), dir.path().as_os_str().to_owned());
        let err = guard(&env).unwrap_err();
        assert!(format!("{err:#}").contains("exposes 2 ICDs"), "{err:#}");

        env.insert("VK_LOADER_DRIVERS_SELECT".into(), "*radeon*".into());
        guard(&env).unwrap();

        env.insert(
            "VK_DRIVER_FILES".into(),
            dir.path().join("missing.json").into_os_string(),
        );
        env.remove(OsStr::new("VK_LOADER_DRIVERS_SELECT"));
        let err = guard(&env).unwrap_err();
        assert!(
            format!("{err:#}").contains("missing or not a regular file"),
            "{err:#}"
        );
    }

    #[test]
    fn an_empty_driver_files_value_names_no_manifest() {
        let mut env = Env::new();
        env.insert("VK_DRIVER_FILES".into(), "".into());
        let err = guard(&env).unwrap_err();
        assert!(format!("{err:#}").contains("exposes no ICD"), "{err:#}");
    }

    #[test]
    fn a_bare_library_path_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = manifest(dir.path(), "radeon.json", "libradeon.so");
        let err = library_path(&path).unwrap_err();
        assert!(format!("{err:#}").contains("bare library_path"), "{err:#}");
    }

    #[test]
    fn the_component_hashes_manifests_and_their_libraries() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("libradeon.so"), "first").unwrap();
        manifest(dir.path(), "radeon.json", "./libradeon.so");
        let mut env = Env::new();
        env.insert(
            "VK_DRIVER_FILES".into(),
            dir.path().join("radeon.json").into_os_string(),
        );
        let first = component(&env).unwrap();
        fs::write(dir.path().join("libradeon.so"), "second").unwrap();
        assert_ne!(first, component(&env).unwrap());
    }
}
