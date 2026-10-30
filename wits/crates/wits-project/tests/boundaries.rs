//! Crate-boundary checks for `wits-project`.
//!
//! The project layer may use core's floor and `git`, but not `forge` or
//! `worktree`: those are command concerns, and only the CLI's own subsystems
//! need them. Cargo cannot enforce that split because all three live in
//! `wits-core`, so the check is a scan of the sources for the rule in
//! `docs/reference/project-containment.rst` ("What crosses, and what does
//! not"). Keeping it as a test puts it in the same `cargo test --workspace`
//! gate as everything else.

use std::fs;
use std::path::{Path, PathBuf};

/// The `wits_core` modules the project layer must not name.
const FORBIDDEN: &[&str] = &["forge", "worktree"];

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

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `text` without its `//` and `/* */` comments, which name no code. String
/// literals are not parsed: a `//` inside one drops the rest of its line, which
/// can only hide what follows it there.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix("//") {
            rest = after.find('\n').map_or("", |end| &after[end..]);
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.find("*/").map_or("", |end| &after[end + 2..]);
            out.push(' ');
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// `text` with the whitespace around path and group punctuation removed and
/// every other run squeezed to one space, so an import reads the same however
/// it is laid out.
fn squeeze(text: &str) -> String {
    let glue = |c: char| matches!(c, ':' | '{' | '}' | ',');
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() && !out.ends_with(glue) && !glue(c) {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out
}

/// Every way the code in `source` reaches a [`FORBIDDEN`] module: a path
/// through it, an import group naming it, or a glob or alias of `wits_core`,
/// which would hide either. The scan is textual — a guard against accidental
/// use, not a Rust parser.
fn reaches(source: &str) -> Vec<String> {
    let text = squeeze(&strip_comments(source));
    let mut hits = Vec::new();
    for (at, name) in text.match_indices("wits_core") {
        let rest = &text[at + name.len()..];
        if text[..at].chars().next_back().is_some_and(is_ident) || rest.starts_with(is_ident) {
            continue;
        }
        if rest.starts_with(" as ") {
            hits.push("a wits_core alias".to_owned());
        } else if let Some(path) = rest.strip_prefix("::") {
            hits.extend(reached_through(path));
        }
    }
    hits
}

/// What a path continuing `wits_core::` reaches.
fn reached_through(path: &str) -> Vec<String> {
    if path.starts_with('*') {
        return vec!["a wits_core glob".to_owned()];
    }
    if let Some(group) = path.strip_prefix('{') {
        return group_items(group)
            .into_iter()
            .flat_map(|item| match item.strip_prefix("self as ") {
                Some(_) => vec!["a wits_core alias".to_owned()],
                None => reached_through(item),
            })
            .collect();
    }
    let head = path.split(|c| !is_ident(c)).next().unwrap_or_default();
    FORBIDDEN
        .iter()
        .filter(|module| **module == head)
        .map(|module| format!("wits_core::{module}"))
        .collect()
}

/// The comma-separated items of an import group, `group` starting just inside
/// its `{`. A nested group stays whole inside its item.
fn group_items(group: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in group.char_indices() {
        match c {
            '{' => depth += 1,
            '}' if depth == 0 => {
                items.push(&group[start..i]);
                break;
            }
            '}' => depth -= 1,
            ',' if depth == 0 => {
                items.push(&group[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    items.into_iter().filter(|item| !item.is_empty()).collect()
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
            let hits = reaches(&fs::read_to_string(&path).unwrap());
            (!hits.is_empty()).then(|| format!("{} reaches {}", path.display(), hits.join(", ")))
        })
        .collect();

    assert!(
        violations.is_empty(),
        "wits-project crossed a wits-core boundary \
         (docs/reference/project-containment.rst):\n{}",
        violations.join("\n")
    );
}

/// The scan sees through every shape an import can take, and passes the
/// modules the project layer may use — including `git`'s own worktree
/// inventory.
#[test]
fn the_scan_sees_grouped_globbed_and_aliased_imports() {
    let reaching = [
        "use wits_core::forge::Client;",
        "let made = ::wits_core::worktree::create(&repo);",
        "use wits_core::{git, forge};",
        "use wits_core::{\n    forge::{Forge, Review},\n    git,\n};",
        "use wits_core::{\n    git, // the floor\n    forge,\n};",
        "use wits_core::{git, /* hosted */ forge};",
        "use wits_core :: { worktree as wt };",
        "use wits_core::*;",
        "use wits_core::{git, *};",
        "use wits_core::{self as core};",
        "use wits_core as core;",
        "extern crate wits_core as core;",
    ];
    for source in reaching {
        assert!(!reaches(source).is_empty(), "missed: {source}");
    }

    let allowed = [
        "use wits_core::git::{self, Repository, Worktree};",
        "use wits_core::{git, remote::{RemoteRoles, Role}, trunk};",
        "pub use wits_core::remote::{RemoteRoles, Role};",
        "use wits_core::path::expand_tilde;",
        "use wits_core::forgery;",
        "let other = my_wits_core::forge::open();",
        "/// Unlike wits_core::forge, this reads no host.",
        "/* use wits_core::{git, forge}; */",
    ];
    for source in allowed {
        assert_eq!(reaches(source), Vec::<String>::new(), "for {source}");
    }
}
