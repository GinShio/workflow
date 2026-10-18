//! Showing a patch the way git shows its own: coloured and through the user's
//! pager when stdout is a terminal, and as the bare patch otherwise, so a pipe
//! or a redirect gets exactly the text.
//!
//! The colours and the pager are git's — `color.diff.*`, `color.diff` or
//! `color.ui`, and `git var GIT_PAGER` — so a patch from wits reads like one
//! from `git diff`, delta included.

use std::io::IsTerminal as _;

use crate::git::Repository;
use crate::process::Command;

const RESET: &str = "\x1b[m";

/// Show `text`: a unified diff, optionally split into sections by
/// `=== label ===` lines.
pub fn show_patch(repo: &Repository, text: &str) -> anyhow::Result<()> {
    if !std::io::stdout().is_terminal() {
        print!("{text}");
        return Ok(());
    }
    let painted = if repo.colors_terminal("color.diff") {
        Palette::of(repo).paint(text)
    } else {
        text.to_owned()
    };
    match repo.pager().filter(|p| p != "cat") {
        Some(pager) => page(&pager, &painted),
        None => {
            print!("{painted}");
            Ok(())
        }
    }
}

/// Feed `text` to `pager`, run as git runs it: through the shell, with the
/// `LESS` and `LV` defaults git gives its own pager unless the user set them.
fn page(pager: &str, text: &str) -> anyhow::Result<()> {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", pager]).force_run();
    for (key, value) in [("LESS", "FRX"), ("LV", "-c")] {
        if std::env::var_os(key).is_none() {
            cmd.env(key, value);
        }
    }
    cmd.status_with_input(text.as_bytes())?;
    Ok(())
}

/// The escape sequences for each part of a patch, git's `color.diff.<slot>`.
struct Palette {
    meta: String,
    frag: String,
    func: String,
    old: String,
    new: String,
    context: String,
    section: String,
}

impl Palette {
    fn of(repo: &Repository) -> Palette {
        let slot = |name: &str, default: &str| repo.color(&format!("color.diff.{name}"), default);
        let context = match slot("context", "") {
            c if c.is_empty() => slot("plain", ""),
            c => c,
        };
        Palette {
            meta: slot("meta", "bold"),
            frag: slot("frag", "cyan"),
            func: slot("func", ""),
            old: slot("old", "red"),
            new: slot("new", "green"),
            context,
            section: slot("commit", "yellow"),
        }
    }

    /// Colour `text` line by line, by where each line sits: a file's header
    /// runs from its `diff --git` line to the first `@@`, and a hunk's lines
    /// lead with a space, `+`, `-` or `\` — so a `--- ` inside a hunk is a
    /// removed line, not a header.
    fn paint(&self, text: &str) -> String {
        #[derive(PartialEq)]
        enum At {
            Outside,
            Header,
            Hunk,
        }
        let mut at = At::Outside;
        let mut out = String::with_capacity(text.len() * 5 / 4);
        for line in text.split_inclusive('\n') {
            let (body, end) = match line.strip_suffix('\n') {
                Some(body) => (body, "\n"),
                None => (line, ""),
            };
            if body.starts_with("diff --git ") {
                at = At::Header;
            } else if body.starts_with("=== ") {
                at = At::Outside;
                push(&mut out, &self.section, body);
                out.push_str(end);
                continue;
            } else if let Some(ranges) = body.strip_prefix("@@") {
                at = At::Hunk;
                match ranges.find("@@") {
                    Some(close) => {
                        let (frag, func) = body.split_at(close + 4);
                        push(&mut out, &self.frag, frag);
                        push(&mut out, &self.func, func);
                    }
                    None => push(&mut out, &self.frag, body),
                }
                out.push_str(end);
                continue;
            } else if at == At::Hunk
                && !matches!(body.chars().next(), None | Some(' ' | '+' | '-' | '\\'))
            {
                at = At::Outside;
            }
            let colour = match at {
                At::Header => &self.meta,
                At::Hunk => match body.chars().next() {
                    Some('+') => &self.new,
                    Some('-') => &self.old,
                    _ => &self.context,
                },
                At::Outside => "",
            };
            push(&mut out, colour, body);
            out.push_str(end);
        }
        out
    }
}

fn push(out: &mut String, colour: &str, text: &str) {
    if colour.is_empty() || text.is_empty() {
        out.push_str(text);
    } else {
        out.push_str(colour);
        out.push_str(text);
        out.push_str(RESET);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> Palette {
        Palette {
            meta: "<M>".into(),
            frag: "<F>".into(),
            func: "<U>".into(),
            old: "<O>".into(),
            new: "<N>".into(),
            context: String::new(),
            section: "<S>".into(),
        }
    }

    fn painted(text: &str) -> String {
        palette().paint(text).replace(RESET, "</>")
    }

    #[test]
    fn a_header_is_meta_up_to_its_first_hunk() {
        let text = "\
diff --git a/x b/x
index 1..2 100644
--- a/x
+++ b/x
@@ -1,2 +1,2 @@ fn main
 keep
-old
+new
";
        assert_eq!(
            painted(text),
            "\
<M>diff --git a/x b/x</>
<M>index 1..2 100644</>
<M>--- a/x</>
<M>+++ b/x</>
<F>@@ -1,2 +1,2 @@</><U> fn main</>
 keep
<O>-old</>
<N>+new</>
"
        );
    }

    #[test]
    fn dashes_and_pluses_inside_a_hunk_are_content() {
        let text = "\
diff --git a/x b/x
--- a/x
+++ b/x
@@ -1 +1 @@
--- not a header
+++ nor this
\\ No newline at end of file
diff --git a/y b/y
--- a/y
";
        let out = painted(text);
        assert!(out.contains("<O>--- not a header</>"), "{out}");
        assert!(out.contains("<N>+++ nor this</>"), "{out}");
        assert!(out.contains("\\ No newline at end of file\n"), "{out}");
        assert!(
            out.ends_with("<M>diff --git a/y b/y</>\n<M>--- a/y</>\n"),
            "{out}"
        );
    }

    #[test]
    fn sections_and_the_text_between_them_stand_apart() {
        let text = "\
=== A ===
(no change)

=== B ===
diff --git a/x b/x
@@ -1 +1 @@
-a
+b

=== base ===
";
        assert_eq!(
            painted(text),
            "\
<S>=== A ===</>
(no change)

<S>=== B ===</>
<M>diff --git a/x b/x</>
<F>@@ -1 +1 @@</>
<O>-a</>
<N>+b</>

<S>=== base ===</>
"
        );
    }

    #[test]
    fn a_pager_reads_the_painted_patch() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("paged");
        let pager = format!("cat > '{}'", out.display());
        page(&pager, "diff --git a/x b/x\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(out).unwrap(),
            "diff --git a/x b/x\n"
        );
    }
}
