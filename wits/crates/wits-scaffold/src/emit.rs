//! Resolve every edit against the tree, then hand back a patch or write it.
//!
//! Edits are applied to **in-memory buffers**, one per file, and only committed
//! once all of them have resolved. That buys two things: several rules touching
//! one file compose (each anchor is resolved against the buffer as the previous
//! rule left it, so offsets are never stale), and a rule whose anchor has gone
//! missing aborts the run with the tree untouched rather than leaving it half
//! scaffolded.
//!
//! ## Why no idempotence check
//!
//! Running twice inserts twice; nothing here detects that. That is deliberate —
//! a probe for "is this already present" is a second description of the same
//! text that drifts away from the first. The safety net is instead that `--patch`
//! is the default: the review step is a diff the user reads, and `git apply`
//! refuses a patch whose context no longer matches.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::plan::{Action, Edit};

/// What the run produced, for the report.
#[derive(Debug, Default)]
pub struct Report {
    pub created: Vec<String>,
    pub edited: Vec<String>,
    /// `(site, note)` — anything about a placement the user should check.
    pub notes: Vec<(String, String)>,
    /// The unified diff, empty when writing in place.
    pub patch: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// Print a unified diff and change nothing. The default: a patch can be
    /// read before it lands, and applies hunk by hunk if part of it has rotted.
    Patch,
    /// Write the files.
    Write,
}

/// Apply `edits` under `root`.
pub fn apply(root: &Path, edits: &[Edit], output: Output) -> Result<Report> {
    let mut report = Report::default();
    // Insertion order matters: the buffers are diffed in the order the
    // catalogue first touched each file, which keeps a patch stable run to run.
    let mut buffers: Vec<(String, Buffer)> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();

    for edit in edits {
        let slot = match index.get(&edit.path) {
            Some(&slot) => slot,
            None => {
                let buffer = Buffer::open(root, &edit.path, &edit.action)?;
                buffers.push((edit.path.clone(), buffer));
                index.insert(edit.path.clone(), buffers.len() - 1);
                buffers.len() - 1
            }
        };
        let buffer = &mut buffers[slot].1;

        match &edit.action {
            Action::Create => {
                buffer.current = edit.text.clone();
                report.created.push(edit.path.clone());
            }
            Action::Insert { anchor, sort_line } => {
                let placed = anchor
                    .locate(&buffer.current, sort_line.as_deref())
                    .with_context(|| format!("{}: {}", edit.path, edit.what))?;
                for note in placed.notes {
                    report
                        .notes
                        .push((format!("{}: {}", edit.path, edit.what), note));
                }
                buffer
                    .current
                    .insert_str(char_boundary(&buffer.current, placed.offset)?, &edit.text);
                report.edited.push(format!("{}: {}", edit.path, edit.what));
            }
        }
    }

    match output {
        Output::Patch => {
            for (path, buffer) in &buffers {
                report.patch.push_str(&unified_diff(path, buffer));
            }
        }
        Output::Write => {
            for (path, buffer) in &buffers {
                if buffer.current == buffer.original {
                    continue;
                }
                let target = root.join(path);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("cannot create directory {}", parent.display()))?;
                }
                std::fs::write(&target, &buffer.current)
                    .with_context(|| format!("cannot write {}", target.display()))?;
            }
        }
    }
    Ok(report)
}

struct Buffer {
    original: String,
    current: String,
    /// True when the file did not exist, so the diff is spelled as an addition.
    is_new: bool,
}

impl Buffer {
    fn open(root: &Path, rel: &str, action: &Action) -> Result<Self> {
        let path: PathBuf = root.join(rel);
        match action {
            Action::Create => {
                if path.exists() {
                    // The rule claims to author the whole file. Overwriting a
                    // hand-edited one would silently discard work, and there is
                    // no way to tell that from a re-run.
                    bail!(
                        "{rel} already exists; a create rule will not overwrite it — remove it \
                         first if regenerating is what you want"
                    );
                }
                Ok(Self {
                    original: String::new(),
                    current: String::new(),
                    is_new: true,
                })
            }
            Action::Insert { .. } => {
                let text = std::fs::read_to_string(&path).with_context(|| {
                    format!(
                        "{rel} does not exist under {} — the catalogue and the tree disagree",
                        root.display()
                    )
                })?;
                Ok(Self {
                    original: text.clone(),
                    current: text,
                    is_new: false,
                })
            }
        }
    }
}

/// A `git apply`-able diff for one file.
///
/// A new file needs the `new file mode` extended header, not just `--- /dev/null`.
/// Once `diff --git` is present git takes creation from the extended headers and
/// strips a leading path component off the `---` line, so `/dev/null` alone
/// arrives as `dev/null` and the whole patch is refused.
fn unified_diff(path: &str, buffer: &Buffer) -> String {
    if buffer.current == buffer.original {
        return String::new();
    }
    let (extended, from) = if buffer.is_new {
        ("new file mode 100644\n", "/dev/null".to_owned())
    } else {
        ("", format!("a/{path}"))
    };
    let diff = similar::TextDiff::from_lines(&buffer.original, &buffer.current);
    let body = diff
        .unified_diff()
        .context_radius(3)
        .header(&from, &format!("b/{path}"))
        .to_string();
    format!("diff --git a/{path} b/{path}\n{extended}{body}")
}

/// Byte offsets come from regex matches on the same text, so they are already on
/// boundaries; this only turns a stale offset into an error instead of a panic.
fn char_boundary(text: &str, offset: usize) -> Result<usize> {
    if offset > text.len() || !text.is_char_boundary(offset) {
        bail!("computed insertion offset {offset} is not a character boundary");
    }
    Ok(offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anchor::{self, AnchorSpec};

    fn insert_at_eof(path: &str, what: &str, text: &str) -> Edit {
        Edit {
            path: path.to_owned(),
            what: what.to_owned(),
            text: text.to_owned(),
            action: Action::Insert {
                anchor: anchor::compile(&AnchorSpec {
                    eof: true,
                    ..Default::default()
                })
                .unwrap(),
                sort_line: None,
            },
        }
    }

    fn create(path: &str, text: &str) -> Edit {
        Edit {
            path: path.to_owned(),
            what: "generated document".to_owned(),
            text: text.to_owned(),
            action: Action::Create,
        }
    }

    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        dir
    }

    fn git(dir: &tempfile::TempDir, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .status()
            .expect("git must be on PATH for this test");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn write_puts_the_text_in_the_file() {
        let dir = tree(&[("list.txt", "a\n")]);
        let edits = vec![insert_at_eof("list.txt", "entry", "b\n")];
        apply(dir.path(), &edits, Output::Write).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("list.txt")).unwrap(),
            "a\nb\n"
        );
    }

    #[test]
    fn patch_changes_nothing_on_disk() {
        let dir = tree(&[("list.txt", "a\n")]);
        let edits = vec![insert_at_eof("list.txt", "entry", "b\n")];
        let report = apply(dir.path(), &edits, Output::Patch).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("list.txt")).unwrap(),
            "a\n"
        );
        assert!(report.patch.contains("diff --git a/list.txt b/list.txt"));
        assert!(report.patch.contains("+b"));
    }

    #[test]
    fn several_edits_to_one_file_compose() {
        let dir = tree(&[("list.txt", "a\n")]);
        let edits = vec![
            insert_at_eof("list.txt", "first", "b\n"),
            insert_at_eof("list.txt", "second", "c\n"),
        ];
        apply(dir.path(), &edits, Output::Write).unwrap();
        // The second anchor resolved against the buffer the first left behind.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("list.txt")).unwrap(),
            "a\nb\nc\n"
        );
    }

    #[test]
    fn a_missing_anchor_leaves_the_tree_untouched() {
        let dir = tree(&[("list.txt", "a\n"), ("other.txt", "x\n")]);
        let edits = vec![
            insert_at_eof("list.txt", "fine", "b\n"),
            Edit {
                path: "other.txt".to_owned(),
                what: "doomed".to_owned(),
                text: "y\n".to_owned(),
                action: Action::Insert {
                    anchor: anchor::compile(&AnchorSpec {
                        close: Some("^nowhere$".to_owned()),
                        ..Default::default()
                    })
                    .unwrap(),
                    sort_line: None,
                },
            },
        ];
        assert!(apply(dir.path(), &edits, Output::Write).is_err());
        // The earlier, resolvable edit must not have been committed.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("list.txt")).unwrap(),
            "a\n"
        );
    }

    #[test]
    fn a_missing_file_names_the_disagreement() {
        let dir = tree(&[]);
        let edits = vec![insert_at_eof("absent.txt", "entry", "b\n")];
        let err = format!(
            "{:#}",
            apply(dir.path(), &edits, Output::Write).unwrap_err()
        );
        assert!(
            err.contains("catalogue and the tree disagree"),
            "got: {err}"
        );
    }

    #[test]
    fn create_writes_a_new_file_and_diffs_against_dev_null() {
        let dir = tree(&[]);
        let edits = vec![create("generated/item.txt", "begin\nend\n")];
        let report = apply(dir.path(), &edits, Output::Patch).unwrap();
        assert!(report.patch.contains("--- /dev/null"));
        assert_eq!(report.created, vec!["generated/item.txt"]);

        apply(dir.path(), &edits, Output::Write).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("generated/item.txt")).unwrap(),
            "begin\nend\n"
        );
    }

    /// The patch is the default output and the only review step, so "git accepts
    /// it" is the actual contract — asserting on the diff text instead is what let
    /// a created file ship a patch git refused outright.
    #[test]
    fn the_patch_is_one_git_will_apply() {
        let dir = tree(&[("list.txt", "a\n")]);
        git(&dir, &["init", "-q"]);
        git(&dir, &["add", "list.txt"]);
        git(
            &dir,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "base",
            ],
        );

        // Both actions in one patch: a creation needs an extended header that an
        // insertion does not, and git rejects the whole patch over one bad hunk.
        let edits = vec![
            insert_at_eof("list.txt", "entry", "b\n"),
            create("generated/item.txt", "begin\nend\n"),
        ];
        let patch = apply(dir.path(), &edits, Output::Patch).unwrap().patch;
        std::fs::write(dir.path().join("p.patch"), &patch).unwrap();

        let out = std::process::Command::new("git")
            .args(["apply", "--check", "p.patch"])
            .current_dir(dir.path())
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git refused the patch: {}\n--- patch ---\n{patch}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn create_refuses_to_clobber() {
        let dir = tree(&[("existing.txt", "hand written\n")]);
        let edits = vec![Edit {
            path: "existing.txt".to_owned(),
            what: "generated document".to_owned(),
            text: "generated\n".to_owned(),
            action: Action::Create,
        }];
        let err = format!(
            "{:#}",
            apply(dir.path(), &edits, Output::Write).unwrap_err()
        );
        assert!(err.contains("already exists"), "got: {err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("existing.txt")).unwrap(),
            "hand written\n"
        );
    }

    #[test]
    fn placement_notes_reach_the_report() {
        let dir = tree(&[("items.txt", "    enum E\n        FIRST_A,\n};\n")]);
        let edits = vec![Edit {
            path: "items.txt".to_owned(),
            what: "id".to_owned(),
            text: "        GROUP_THING,\n".to_owned(),
            action: Action::Insert {
                anchor: anchor::compile(&AnchorSpec {
                    scope: vec!["enum E".to_owned()],
                    close: Some(r"^\};$".to_owned()),
                    key: Some(r"^\s*([A-Z0-9_]+),".to_owned()),
                    group: Some("^([A-Z]+)_".to_owned()),
                    ..Default::default()
                })
                .unwrap(),
                sort_line: Some("        GROUP_THING,".to_owned()),
            },
        }];
        let report = apply(dir.path(), &edits, Output::Patch).unwrap();
        assert!(!report.notes.is_empty(), "a new section must be reported");
        assert!(report.notes[0].0.contains("items.txt: id"));
    }

    #[test]
    fn an_unchanged_file_produces_no_hunk() {
        let dir = tree(&[("list.txt", "a\n")]);
        let report = apply(dir.path(), &[], Output::Patch).unwrap();
        assert!(report.patch.is_empty());
    }
}
