//! Crate-boundary checks for `wits-project`.
//!
//! The project layer may use core's floor and `git`, but not `forge` or
//! `worktree`: those are command concerns, and only the CLI's own subsystems
//! need them. Cargo cannot enforce that split because all three live in
//! `wits-core`, so the check is the source grep from the architecture handoff
//! (§1, §11.3). Keeping it as a test puts it in the same `cargo test
//! --workspace` gate as everything else.

use std::fs;
use std::path::{Path, PathBuf};

/// A module path the project layer must not name. The scan is deliberately
/// textual, matching the handoff's grep: it is a guard against accidental use,
/// not a Rust parser. Do not spell a forbidden path in `src` comments either;
/// describe the boundary here or in the handoff instead.
const FORBIDDEN: &[&str] = &["wits_core::forge", "wits_core::worktree"];

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_project_layer_does_not_reach_forge_or_worktree() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    rust_sources(&src, &mut sources);
    assert!(
        !sources.is_empty(),
        "no sources found under {}",
        src.display()
    );

    let violations: Vec<String> = sources
        .into_iter()
        .filter_map(|path| {
            let text = fs::read_to_string(&path).unwrap();
            let hits: Vec<&str> = FORBIDDEN
                .iter()
                .copied()
                .filter(|needle| text.contains(needle))
                .collect();
            (!hits.is_empty()).then(|| format!("{} names {}", path.display(), hits.join(", ")))
        })
        .collect();

    assert!(
        violations.is_empty(),
        "wits-project crossed a wits-core boundary:\n{}",
        violations.join("\n")
    );
}
