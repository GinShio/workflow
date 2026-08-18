//! `skip`: the paths a checkout never materialises.
//!
//! A repo's `skip` is an ordered, gitignore-style pattern list saying what to
//! *leave out*, where a leading `!` re-includes. It is what makes a `from`
//! borrow usable: the borrower keeps its own copy of the component
//! unmaterialised instead of shadowing the checkout it borrowed. It is equally
//! useful alone — a monorepo you only build one component of.
//!
//! ## Why one field drives two git mechanisms
//!
//! `sparse-checkout` alone cannot mask a **materialised submodule**: git tries
//! to remove the directory, fails because the submodule's content and `.git` are
//! in it, and leaves the gitlink un-skipped with only a `warning: unable to
//! rmdir`. The mask has to be `submodule deinit` *first*, then sparse — after
//! which the gitlink is `S`, the directory is gone, and the tree is clean.
//! Reversing the order silently does nothing. So `skip` is one declaration whose
//! mechanism is settled per path at the moment a checkout is built, exactly as a
//! repo's *kind* is inferred rather than declared.
//!
//! ## Declared, applied once, then only verified
//!
//! Applying the mask to a tree that already has the content is destructive, and
//! `update` never touches a working tree (`docs/project/design.md` §7). So the
//! split is: `clone` **applies** it (the tree is ours, still being built, so
//! removing what config says not to keep is finishing construction, not
//! repairing reality), while `update` and `project --check` only **verify** and
//! fail loudly. Converting an existing checkout is your `git` call —
//! [`remedy`] prints exactly which.
//!
//! Verification is deliberately **behavioural, not textual**: it asks "is
//! anything a skip pattern excludes still materialised?", never "does the sparse
//! file look like what I would have written". So your own extra sparse patterns
//! are legal and invisible here, and reordering or hand-editing them is too.

use crate::git::Repository;

/// The sparse-checkout patterns realising a `skip` list.
///
/// sparse-checkout speaks the *opposite* language — its patterns say what to
/// **keep** — so the translation is `/*` (keep everything) followed by every
/// `skip` entry with its leading `!` toggled. Order is preserved because both
/// languages resolve a path by its last matching pattern.
///
/// Non-cone is not a choice: cone mode cannot express an exclusion at all. It
/// costs nothing here — nested re-inclusion works (git matches each index entry
/// against the list, so unlike gitignore's directory walk a `!` under an
/// excluded directory *does* take effect), and git emits no deprecation warning.
pub fn sparse_patterns(skip: &[String]) -> Vec<String> {
    let mut out = vec!["/*".to_owned()];
    for entry in skip {
        out.push(match entry.strip_prefix('!') {
            Some(reincluded) => reincluded.to_owned(),
            None => format!("!{entry}"),
        });
    }
    out
}

/// One parsed `skip` entry, as a git **pathspec**: sparse's leading `/` anchor
/// dropped (a pathspec is repo-relative already) and any trailing `/` with it.
struct Entry {
    /// A `!` entry re-includes rather than excludes.
    excluding: bool,
    spec: String,
    /// No glob metacharacter, so the entry names exactly one path and can be
    /// reasoned about by prefix alone.
    literal: bool,
}

fn entries(skip: &[String]) -> Vec<Entry> {
    skip.iter()
        .map(|raw| {
            let (excluding, body) = match raw.strip_prefix('!') {
                Some(rest) => (false, rest),
                None => (true, raw.as_str()),
            };
            let spec = body
                .trim_start_matches('/')
                .trim_end_matches('/')
                .to_owned();
            Entry {
                excluding,
                literal: is_literal(&spec),
                spec,
            }
        })
        .filter(|e| !e.spec.is_empty())
        .collect()
}

/// Are the checkout's sparse patterns already exactly the ones `skip` asks for?
///
/// This is the one place a *textual* comparison is right, and it asks a different
/// question from [`violations`]: not "is the mask in force" but "may we write this
/// file at all". Anything else already in there belongs to whoever put it there —
/// a bootstrap script that established a sparse cone, or you — and
/// `sparse-checkout set` replaces the *whole* list, which would silently expand a
/// checkout that was deliberately narrow. Cone mode can never match, and that is
/// correct: it cannot express an exclusion, so there is nothing safe to write.
pub fn sparse_already_ours(git: &Repository, skip: &[String]) -> bool {
    git.sparse_list() == sparse_patterns(skip)
}

/// Does the list leave `path` out of the checkout?
///
/// Both `skip` and sparse-checkout resolve a path by its **last matching**
/// pattern, so this walks the list to the end rather than stopping at the first
/// hit: `["/vendor", "!/vendor/keep.c"]` excludes `vendor/blob` and keeps
/// `vendor/keep.c`. A non-literal entry we cannot match without a glob engine is
/// treated as no answer at all (`None`) — better to verify nothing than to
/// report a violation git would not agree with.
fn is_excluded(entries: &[Entry], path: &str) -> Option<bool> {
    let mut verdict = false;
    for entry in entries {
        if !entry.literal {
            return None;
        }
        if covers(&entry.spec, path) {
            verdict = entry.excluding;
        }
    }
    Some(verdict)
}

/// The submodules of `git` the `skip` list leaves out and that are currently
/// materialised — the ones `deinit` has to precede the sparse write for.
pub fn materialised_skipped_submodules(git: &Repository, skip: &[String]) -> Vec<String> {
    let entries = entries(skip);
    git.materialised_submodules()
        .into_iter()
        .map(|sub| sub.path)
        .filter(|sub| is_excluded(&entries, sub) == Some(true))
        .collect()
}

/// Does pathspec `spec` cover `path`? The two shapes a literal entry can take:
/// the same path, or a directory containing it.
fn covers(spec: &str, path: &str) -> bool {
    path == spec || path.starts_with(&format!("{spec}/"))
}

/// Every way the checkout currently contradicts its declared `skip`, as reader
/// facing lines. Empty means the mask is in force.
///
/// Two independent facts, because each catches a state the other misses:
///
/// - an entry the list excludes **wholly** must not exist on disk — this catches
///   a `deinit` with no sparse write, which leaves an empty directory behind (and
///   a build system probing `if(EXISTS …)` then finds the empty directory);
/// - every index entry under a skipped path must be tagged `S` (skip-worktree)
///   unless the list re-includes it — this catches the reverse, a directory
///   removed by hand without the sparse write, which git reports as a deletion.
///
/// What is deliberately *not* checked is a glob entry, or anything under one:
/// deciding those needs gitignore's matcher, and a second implementation of it
/// would disagree with git's in exactly the corners that matter. Verification
/// under-reports rather than guesses.
pub fn violations(git: &Repository, skip: &[String]) -> Vec<String> {
    let entries = entries(skip);
    let mut out = Vec::new();

    for (i, entry) in entries.iter().enumerate() {
        if !entry.excluding || !entry.literal {
            continue;
        }
        // Whether the entry is wholly gone or merely thinned depends on what
        // comes *after* it, since the last match wins.
        let reincluded_below = entries[i + 1..]
            .iter()
            .any(|later| !later.excluding && covers(&entry.spec, &later.spec));
        if !reincluded_below && git.path().join(&entry.spec).exists() {
            out.push(format!(
                "skipped path '{}' is materialised (it should not be checked out)",
                entry.spec
            ));
        }
        for (tag, path) in git.ls_files_status(std::slice::from_ref(&entry.spec)) {
            if tag != 'S' && is_excluded(&entries, &path) == Some(true) {
                out.push(format!(
                    "skipped path '{path}' is not marked skip-worktree (index tag '{tag}')"
                ));
            }
        }
    }
    out
}

/// The git commands that would bring a checkout in line with its `skip`, in the
/// order that works. Printed under `-v` beside the error, so the fix is one
/// copy-paste and the destructive step stays yours to run.
pub fn remedy(git: &Repository, skip: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let subs = materialised_skipped_submodules(git, skip);
    if !subs.is_empty() {
        out.push(format!("git submodule deinit -f -- {}", subs.join(" ")));
    }
    let quoted: Vec<String> = sparse_patterns(skip)
        .iter()
        .map(|p| format!("'{p}'"))
        .collect();
    out.push(format!(
        "git sparse-checkout set --no-cone {}",
        quoted.join(" ")
    ));
    out
}

/// A pattern with no glob metacharacter, so it names exactly one path and can be
/// checked on disk directly.
fn is_literal(spec: &str) -> bool {
    !spec.contains(['*', '?', '[', ']'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inversion_toggles_each_entry_and_keeps_order() {
        let skip = vec![
            "/third_party/engine".to_owned(),
            "/vendor".to_owned(),
            "!/vendor/keep.c".to_owned(),
        ];
        assert_eq!(
            sparse_patterns(&skip),
            vec!["/*", "!/third_party/engine", "!/vendor", "/vendor/keep.c"]
        );
    }

    #[test]
    fn empty_skip_still_keeps_everything() {
        assert_eq!(sparse_patterns(&[]), vec!["/*"]);
    }

    /// Parsing normalises an entry to a repo-relative pathspec: `!` becomes a
    /// flag, sparse's `/` anchor and any trailing `/` are dropped, and an entry
    /// that says nothing at all is discarded.
    #[test]
    fn entries_normalise_the_marker_and_the_anchors() {
        let parsed = entries(&[
            "/third_party/engine".to_owned(),
            "!/vendor/keep.c".to_owned(),
            "bigdata/".to_owned(),
            "/".to_owned(),
        ]);
        let shape: Vec<(bool, &str)> = parsed
            .iter()
            .map(|e| (e.excluding, e.spec.as_str()))
            .collect();
        assert_eq!(
            shape,
            vec![
                (true, "third_party/engine"),
                (false, "vendor/keep.c"),
                (true, "bigdata"),
            ]
        );
    }

    /// The list resolves a path by its *last* match, so a `!` after an exclusion
    /// carves a hole in it — and a glob anywhere makes the answer unavailable
    /// rather than guessed.
    #[test]
    fn exclusion_verdict_follows_the_last_match() {
        let carved = entries(&["/vendor".to_owned(), "!/vendor/keep.c".to_owned()]);
        assert_eq!(is_excluded(&carved, "vendor"), Some(true));
        assert_eq!(is_excluded(&carved, "vendor/blob/big.bin"), Some(true));
        assert_eq!(is_excluded(&carved, "vendor/keep.c"), Some(false));
        assert_eq!(is_excluded(&carved, "src/main.c"), Some(false));

        let globbed = entries(&["/plugins/*".to_owned()]);
        assert_eq!(is_excluded(&globbed, "plugins/x"), None);
    }

    #[test]
    fn literal_detection_excludes_globs() {
        assert!(is_literal("third_party/engine"));
        assert!(!is_literal("src/plugins/*"));
        assert!(!is_literal("a[bc]"));
    }

    #[test]
    fn covers_matches_the_path_and_its_parents_only() {
        assert!(covers("third_party/engine", "third_party/engine"));
        assert!(covers("third_party", "third_party/engine"));
        assert!(!covers("third_party/eng", "third_party/engine"));
        assert!(!covers("third_party/engine/x", "third_party/engine"));
    }
}
