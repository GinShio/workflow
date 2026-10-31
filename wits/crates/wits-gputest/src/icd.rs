//! The Vulkan ICD set: what the composed environment exposes, how the loader
//! selects among it, and the content that identifies it.
//!
//! `VK_DRIVER_FILES` wins over `VK_ICD_FILENAMES`; a directory entry expands to
//! its `*.json` manifests. `VK_LOADER_DRIVERS_SELECT` is the loader's own rule:
//! a comma-delimited list of case-insensitive globs matched against the
//! manifest's *filename*, never its path or library. A manifest's
//! `library_path` is relative to the manifest's directory; a bare filename is
//! refused because the loader would search system paths and identity could not
//! name what loaded.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use regex::Regex;

use wits_actions::Env;

use crate::content;
use crate::util;

/// The manifests the environment names, before selection.
pub fn manifests(env: &Env) -> Result<Vec<PathBuf>> {
    let paths = if util::env_os(env, "VK_DRIVER_FILES").is_some() {
        util::env_paths(env, "VK_DRIVER_FILES")
    } else {
        util::env_paths(env, "VK_ICD_FILENAMES")
    };

    let mut out = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&path)
                .with_context(|| format!("reading ICD directory {}", path.display()))?
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    let path = entry.path();
                    (path.extension().and_then(|e| e.to_str()) == Some("json")).then_some(path)
                })
                .collect();
            entries.sort_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
            out.extend(entries);
        } else {
            out.push(path);
        }
    }
    out.sort_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
    out.dedup();
    Ok(out)
}

/// Apply the loader's selector. `None` means no selector and every manifest.
pub fn select(all: &[PathBuf], select: Option<&str>) -> Result<Vec<PathBuf>> {
    let Some(select) = select else {
        return Ok(all.to_vec());
    };
    let patterns: Vec<Regex> = select
        .split(',')
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .map(glob_regex)
        .collect::<Result<_>>()?;
    Ok(all
        .iter()
        .filter(|manifest| {
            let name = manifest
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default();
            patterns.iter().any(|pattern| pattern.is_match(&name))
        })
        .cloned()
        .collect())
}

/// The driver guard: exactly one selected ICD.
pub fn guard(env: &Env) -> Result<PathBuf> {
    let all = manifests(env)?;
    if all.is_empty() {
        bail!(
            "driver exposes no ICD: the composed environment names no \
             VK_DRIVER_FILES or VK_ICD_FILENAMES manifest"
        );
    }
    let selector = util::env_text(env, "VK_LOADER_DRIVERS_SELECT")?;
    let selected = select(&all, selector)?;
    let names = || {
        all.iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match selected.len() {
        1 => Ok(selected.into_iter().next().expect("length 1")),
        0 => match selector {
            Some(selector) => bail!(
                "VK_LOADER_DRIVERS_SELECT={selector:?} matched 0 ICDs (candidates: {})",
                names()
            ),
            None => unreachable!("a selector-less non-empty set cannot select zero"),
        },
        n => match selector {
            Some(selector) => bail!(
                "VK_LOADER_DRIVERS_SELECT={selector:?} matched {n} ICDs ({}); \
                 a deqp case must load exactly one",
                names()
            ),
            None => bail!(
                "the composed environment exposes {n} ICDs ({}); \
                 set driver.select to name exactly one for a deqp case",
                names()
            ),
        },
    }
}

/// The `driver.icd` component: the selected manifests and the libraries they
/// The driver.icd component: the selected manifests and the libraries they
/// name, as a content set. It does not require exactly one manifest; the kind's
/// guard does.
pub fn component(env: &Env, cache: &mut content::HashCache) -> Result<Vec<(String, String)>> {
    let all = manifests(env)?;
    let selector = util::env_text(env, "VK_LOADER_DRIVERS_SELECT")?;
    let selected = select(&all, selector)?;
    let mut entries = Vec::new();
    for manifest in selected {
        let manifest_hash = cache.hash_file(&manifest)?;
        entries.push((
            format!("manifest:{}", manifest.to_string_lossy()),
            manifest_hash,
        ));
        if let Some(library) = library_path(&manifest)? {
            let hash = cache.hash_file(&library).with_context(|| {
                format!(
                    "hashing the library {} named by {}",
                    library.display(),
                    manifest.display()
                )
            })?;
            entries.push((format!("library:{}", library.to_string_lossy()), hash));
        }
    }
    Ok(entries)
}

/// The library a manifest names, resolved against the manifest's directory. A
/// bare filename is refused: the loader would search the system paths, and the
/// identity could not name what loaded.
fn library_path(manifest: &Path) -> Result<Option<PathBuf>> {
    let bytes = util::read_bytes(manifest)?;
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
    let library = Path::new(library);
    if !library.to_string_lossy().contains('/') {
        bail!(
            "ICD manifest {} names a bare library_path {:?}; the loader would search system paths, \
             so the identity cannot name what loaded",
            manifest.display(),
            library
        );
    }
    if library.is_absolute() {
        Ok(Some(library.to_path_buf()))
    } else {
        let parent = manifest
            .parent()
            .with_context(|| format!("{} has no parent directory", manifest.display()))?;
        Ok(Some(parent.join(library)))
    }
}

/// The loader's glob rule, compiled for a filename. `*`, `?`, `[…]` with `!`
/// or `^` negation; every other character is literal. Matching is
/// case-insensitive.
fn glob_regex(pattern: &str) -> Result<Regex> {
    let mut out = String::from("(?i)^");
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            '[' => {
                let mut class = String::new();
                if let Some(&first) = chars.peek() {
                    if first == '!' || first == '^' {
                        class.push('^');
                        chars.next();
                    }
                }
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == ']' {
                        closed = true;
                        break;
                    }
                    match c {
                        '\\' => class.push_str("\\\\"),
                        '-' => class.push('-'),
                        c => class.push_str(&regex::escape(&c.to_string())),
                    }
                }
                if closed {
                    out.push('[');
                    out.push_str(&class);
                    out.push(']');
                } else {
                    out.push_str("\\[");
                    out.push_str(&regex::escape(&class));
                }
            }
            '\\' => {
                if let Some(next) = chars.next() {
                    out.push_str(&regex::escape(&next.to_string()));
                } else {
                    out.push_str("\\\\");
                }
            }
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    out.push('$');
    Regex::new(&out).with_context(|| format!("compiling the loader glob {pattern:?}"))
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

    #[test]
    fn a_directory_expands_to_json_manifests_only() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), "radeon.json", "./libradeon.so");
        manifest(dir.path(), "lvp.json", "./liblvp.so");
        fs::write(dir.path().join("README"), "").unwrap();
        let mut env = Env::new();
        env.insert("VK_DRIVER_FILES".into(), dir.path().as_os_str().to_owned());
        let manifests = manifests(&env).unwrap();
        assert_eq!(manifests.len(), 2);
        assert!(manifests.iter().all(|p| p.extension().unwrap() == "json"));
    }

    #[test]
    fn selection_matches_the_filename_case_insensitively() {
        let all = vec![
            PathBuf::from("/a/radeon_icd.x86_64.json"),
            PathBuf::from("/a/lvp_icd.x86_64.json"),
        ];
        let selected = select(&all, Some("*RADEON*")).unwrap();
        assert_eq!(selected, [PathBuf::from("/a/radeon_icd.x86_64.json")]);
        assert!(select(&all, Some("*amd*")).unwrap().is_empty());
        assert_eq!(select(&all, None).unwrap().len(), 2);
    }

    #[test]
    fn the_guard_requires_exactly_one_selected_manifest() {
        let dir = tempfile::tempdir().unwrap();
        manifest(dir.path(), "radeon.json", "./libradeon.so");
        manifest(dir.path(), "lvp.json", "./liblvp.so");
        let mut env = Env::new();
        env.insert("VK_DRIVER_FILES".into(), dir.path().as_os_str().to_owned());
        let err = guard(&env).unwrap_err();
        assert!(format!("{err:#}").contains("exposes 2 ICDs"), "{err:#}");

        env.insert("VK_LOADER_DRIVERS_SELECT".into(), "*radeon*".into());
        assert_eq!(guard(&env).unwrap(), dir.path().join("radeon.json"));
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
        let mut cache = content::HashCache::new();
        let first = content::hash_set(&component(&env, &mut cache).unwrap()).unwrap();
        fs::write(dir.path().join("libradeon.so"), "second").unwrap();
        // A fresh cache: the per-command cache is not expected to notice a file
        // changing during one command.
        let mut cache = content::HashCache::new();
        let second = content::hash_set(&component(&env, &mut cache).unwrap()).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn a_glob_with_a_class_and_a_question_mark_matches_like_fn_match() {
        let all = vec![
            PathBuf::from("/a/radeon_icd.x86_64.json"),
            PathBuf::from("/a/lvp_icd.x86_64.json"),
        ];
        assert_eq!(
            select(&all, Some("radeon_icd.??????.json")).unwrap(),
            [PathBuf::from("/a/radeon_icd.x86_64.json")]
        );
        assert_eq!(
            select(&all, Some("[rl]vp_icd*")).unwrap(),
            [PathBuf::from("/a/lvp_icd.x86_64.json")]
        );
    }
}
