//! `wits review edit` — write a review in the patch it is about.
//!
//! The draft's JSON is the contract an agent writes; a person reviewing by hand
//! wants the patch in front of them and to type under the line they mean. So
//! `edit` renders the MR's current review point as a quoted patch with the
//! discussion and the draft in place, opens it in git's editor, and reads what
//! was written back as draft actions — the same actions `draft <mr> -` takes.
//!
//! The buffer is quoted text with writing in between:
//!
//! * `> ` quotes the patch and `>> ` the discussion and the tool's own notes;
//!   neither is ever sent.
//! * Unquoted text belongs to the nearest quoted line above it: a patch line (a
//!   comment on that line, or on the file under a file's header), a thread (a
//!   reply), or a `>> draft <id>` marker (that draft's text). Above the first
//!   patch line and outside any thread, it is the review's summary.
//!
//! Patch lines are matched against the patch as rendered, and a comment's
//! position is read off the rendered line, never re-derived from the buffer, so
//! no edit can shift a line number; whole files may be deleted to cut the
//! noise, anything finer is refused. A `>` line that is not the patch's next
//! line is text — a Markdown quote in a comment.
//!
//! What is read back is applied as changes against what was shown, recorded
//! beside the buffer, not as a replacement draft: drafts an agent adds while the
//! editor is open survive, and a summary or draft left alone is not rewritten.
//! A buffer that does not parse is kept, and the next `edit` reopens it.

use std::collections::{HashMap, HashSet};
use std::fs;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use wits_util::forge::{Anchor, Side, Verdict};
use wits_util::git::Repository;
use wits_util::log as wits_log;

use super::lines;
use super::model::{short, Action, Local, Thread, ThreadId};
use super::{local, EditArgs};

pub fn run(repo: &Repository, args: &EditArgs) -> Result<()> {
    let ctx = local(repo)?;
    let id = super::parse_mr_handle(&args.mr)?;
    let info = ctx.store.load_info(&id).with_context(|| {
        format!("MR {id} isn't in the store yet — run `wits review fetch {id}` first")
    })?;
    let snapshot = info.current().with_context(|| {
        format!("MR {id} has no fetched review point — run `wits review fetch {id}` first")
    })?;
    let head = snapshot.head_sha.clone();
    let patch = Patch::parse(&super::diff::patch_text_between(
        &ctx.repo,
        snapshot.fork(),
        &head,
    )?);

    let (buffer, base_path) = ctx.store.edit_paths(&id);
    if args.discard {
        remove(&[&buffer, &base_path]);
    }
    let base = match fs::read_to_string(&base_path) {
        Ok(json) if buffer.exists() => {
            let base: Base = serde_json::from_str(&json)
                .with_context(|| format!("parsing {}", base_path.display()))?;
            anyhow::ensure!(
                base.head == head,
                "an unfinished edit of MR {id}, written against {}, is in {}; the MR has \
                 moved to {} since. Copy what you need from it, then start over with \
                 `wits review edit {id} --discard`",
                short(&base.head),
                buffer.display(),
                short(&head)
            );
            log::info!("MR {id}: reopening the unfinished edit");
            base
        }
        _ => {
            let comments = ctx.store.load_comments(&id);
            let mut draft = ctx.store.load_local(&id)?;
            draft.normalize(&head);
            let title = format!("{} {}", info.mr.display, info.mr.title);
            let view = View {
                id: &id,
                title: &title,
                fork: snapshot.fork(),
                head: &head,
            };
            let (text, base) = render(&ctx.repo, &view, &patch, &comments.threads, &draft);
            fs::write(&buffer, text).with_context(|| format!("writing {}", buffer.display()))?;
            fs::write(&base_path, serde_json::to_string_pretty(&base)?)
                .with_context(|| format!("writing {}", base_path.display()))?;
            base
        }
    };

    let kept = |e: anyhow::Error| {
        anyhow::anyhow!(
            "{e:#}\nThe buffer is kept in {}: `wits review edit {id}` reopens it, `--discard` \
             starts over.",
            buffer.display()
        )
    };
    wits_util::editor::edit(repo, &buffer).map_err(kept)?;
    let text =
        fs::read_to_string(&buffer).with_context(|| format!("reading {}", buffer.display()))?;
    let (actions, verdict) = parse(&text, &patch)
        .and_then(|written| changes(&base, written, &head))
        .map_err(kept)?;

    if actions.is_empty() && verdict.is_none() {
        log::info!("MR {id}: nothing changed");
    } else if wits_log::is_dry_run() {
        if let Some(v) = verdict {
            let v = v.map_or("none", Verdict::display_str);
            wits_log::dry_run(&format!("MR {id}: set the verdict to {v}"));
        }
        for action in &actions {
            wits_log::dry_run(&format!("MR {id}: {}", describe(action)));
        }
    } else {
        let mut draft = ctx.store.load_local(&id)?;
        let summary = actions.iter().map(describe).collect::<Vec<_>>();
        draft.actions.extend(actions);
        if let Some(v) = verdict {
            draft.verdict = v;
        }
        draft.normalize(&head);
        ctx.store.save_local(&id, &draft)?;
        if let Some(v) = verdict {
            log::info!(
                "MR {id}: verdict {}",
                v.map_or("none", Verdict::display_str)
            );
        }
        for line in summary {
            log::info!("MR {id}: {line}");
        }
    }
    remove(&[&buffer, &base_path]);
    Ok(())
}

fn remove(paths: &[&std::path::Path]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

fn describe(action: &Action) -> String {
    match action {
        Action::Comment {
            id: Some(id), body, ..
        } => format!("draft {id} rewritten: {}", first_line(body)),
        Action::Comment {
            file, line, body, ..
        } => match (file, line) {
            (Some(file), Some(line)) => format!("comment on {file}:{line}: {}", first_line(body)),
            (Some(file), None) => format!("comment on {file}: {}", first_line(body)),
            _ => format!("comment: {}", first_line(body)),
        },
        Action::Reply {
            id: Some(id), body, ..
        } => format!("draft {id} rewritten: {}", first_line(body)),
        Action::Reply { thread, body, .. } => {
            format!("reply to thread {thread}: {}", first_line(body))
        }
        Action::Resolve {
            thread, resolved, ..
        } => {
            let verb = if *resolved { "resolve" } else { "reopen" };
            format!("{verb} thread {thread}")
        }
        Action::Summary { body, .. } => format!("summary: {}", first_line(body)),
        Action::Drop { id } => format!("draft {id} dropped"),
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

// ---------------------------------------------------------------------------
// The patch as rendered.
// ---------------------------------------------------------------------------

/// Where a line of the rendered patch sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum At {
    /// A file's header or a hunk header: a comment there is on the file.
    Header { file: usize },
    /// A line of a hunk, on `side` at `line`. A context line is on both sides;
    /// `old` is its pre-image number then.
    Line {
        file: usize,
        hunk: usize,
        side: Side,
        line: u32,
        old: Option<u32>,
    },
    /// `\ No newline at end of file`, which belongs to the line before it.
    Marker,
}

#[derive(Debug)]
struct PatchLine {
    text: String,
    at: At,
}

/// A parsed patch: its lines, and each file's path as a comment names it — the
/// new path, or the old one for a deleted file.
#[derive(Debug, Default)]
struct Patch {
    lines: Vec<PatchLine>,
    files: Vec<String>,
}

impl Patch {
    fn parse(text: &str) -> Patch {
        let mut patch = Patch::default();
        let (mut file, mut hunk) = (0, 0);
        let mut in_hunk = false;
        let (mut old, mut new) = (0u32, 0u32);
        let mut minus: Option<String> = None;
        for line in text.lines() {
            let at = if let Some(rest) = line.strip_prefix("diff --git ") {
                patch.files.push(header_path(rest));
                file = patch.files.len() - 1;
                in_hunk = false;
                minus = None;
                At::Header { file }
            } else if patch.files.is_empty() {
                continue;
            } else if let Some((o, n)) = hunk_starts(line) {
                in_hunk = true;
                hunk += 1;
                (old, new) = (o, n);
                At::Header { file }
            } else if in_hunk {
                let side_line = |side, line, old| At::Line {
                    file,
                    hunk,
                    side,
                    line,
                    old,
                };
                match line.as_bytes().first() {
                    Some(b'+') => {
                        new += 1;
                        side_line(Side::New, new - 1, None)
                    }
                    Some(b'-') => {
                        old += 1;
                        side_line(Side::Old, old - 1, None)
                    }
                    Some(b'\\') => At::Marker,
                    _ => {
                        old += 1;
                        new += 1;
                        side_line(Side::New, new - 1, Some(old - 1))
                    }
                }
            } else {
                if let Some(path) = line.strip_prefix("--- ") {
                    minus = (path != "/dev/null").then(|| field_path(path, "a/"));
                } else if let Some(path) = line.strip_prefix("+++ ") {
                    match path {
                        "/dev/null" => {
                            if let Some(path) = minus.take() {
                                patch.files[file] = path;
                            }
                        }
                        _ => patch.files[file] = field_path(path, "b/"),
                    }
                } else if let Some(path) = line.strip_prefix("rename to ") {
                    patch.files[file] = field_path(path, "");
                }
                At::Header { file }
            };
            patch.lines.push(PatchLine {
                text: line.to_owned(),
                at,
            });
        }
        patch
    }

    fn path(&self, file: usize) -> &str {
        &self.files[file]
    }

    /// The rendered line a comment on `(path, side, line)` sits under.
    fn line_index(&self) -> HashMap<(&str, Side, u32), usize> {
        let mut map = HashMap::new();
        for (i, l) in self.lines.iter().enumerate() {
            if let At::Line {
                file,
                side,
                line,
                old,
                ..
            } = l.at
            {
                map.insert((self.path(file), side, line), i);
                if let Some(old) = old {
                    map.insert((self.path(file), Side::Old, old), i);
                }
            }
        }
        map
    }

    /// The rendered line a comment on a whole file sits under: the last line of
    /// its header.
    fn file_index(&self) -> HashMap<&str, usize> {
        let mut map = HashMap::new();
        let mut hunked = HashSet::new();
        for (i, l) in self.lines.iter().enumerate() {
            if let At::Header { file } = l.at {
                if l.text.starts_with("@@") {
                    hunked.insert(file);
                } else if !hunked.contains(&file) {
                    map.insert(self.path(file), i);
                }
            }
        }
        map
    }
}

/// `@@ -a[,b] +c[,d] @@` → `(a, c)`, each side's first line.
fn hunk_starts(line: &str) -> Option<(u32, u32)> {
    let (ranges, _) = line.strip_prefix("@@ -")?.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let start = |field: &str| field.split(',').next()?.parse().ok();
    Some((start(old)?, start(new)?))
}

/// A path field of a header line, unquoted, without its `a/` or `b/`.
fn field_path(field: &str, prefix: &str) -> String {
    let path = match unquote(field) {
        Some((path, _)) => path,
        None => field.to_owned(),
    };
    path.strip_prefix(prefix).map(str::to_owned).unwrap_or(path)
}

/// The new path a `diff --git` header names: the fallback for a file with no
/// `+++` line, a binary or mode-only change.
fn header_path(rest: &str) -> String {
    if let Some((_, after)) = unquote(rest) {
        return field_path(after.trim_start(), "b/");
    }
    // `a/P b/P`: an unquoted path may hold " b/" itself, so split where the
    // halves agree.
    for (i, _) in rest.match_indices(" b/") {
        if rest.get(2..i) == rest.get(i + 3..) {
            return rest[i + 3..].to_owned();
        }
    }
    match rest.rsplit_once(" b/") {
        Some((_, b)) => b.to_owned(),
        None => rest.to_owned(),
    }
}

/// A C-style quoted string as git writes an unusual path, and what follows it.
fn unquote(s: &str) -> Option<(String, &str)> {
    let body = s.strip_prefix('"')?;
    let mut out = Vec::new();
    let mut chars = body.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((String::from_utf8_lossy(&out).into_owned(), &body[i + 1..])),
            '\\' => {
                let (_, e) = chars.next()?;
                let byte = match e {
                    'a' => 7,
                    'b' => 8,
                    't' => b'\t',
                    'n' => b'\n',
                    'v' => 11,
                    'f' => 12,
                    'r' => b'\r',
                    '0'..='7' => {
                        let digit = |c: char| c.to_digit(8);
                        let (d2, d3) = (chars.next()?.1, chars.next()?.1);
                        (digit(e)? * 64 + digit(d2)? * 8 + digit(d3)?) as u8
                    }
                    other => {
                        out.extend_from_slice(other.encode_utf8(&mut [0; 4]).as_bytes());
                        continue;
                    }
                };
                out.push(byte);
            }
            c => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Rendering.
// ---------------------------------------------------------------------------

/// What a buffer showed, kept beside it: what is read back is measured against
/// this, not against the draft as it stands by then.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Base {
    head: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verdict: Option<Verdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<ShownSummary>,
    /// The drafts shown under a `>> draft` marker, which the buffer may change.
    #[serde(default)]
    drafts: Vec<Action>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ShownSummary {
    id: String,
    body: String,
}

struct View<'a> {
    id: &'a str,
    title: &'a str,
    fork: &'a str,
    head: &'a str,
}

/// Something shown at the top, under a patch line, or at the end.
enum Note<'a> {
    /// A forge thread, with where it sits when that is not its patch line.
    Thread(&'a Thread, Option<String>),
    /// A draft comment, editable.
    Draft(&'a Action),
    /// A draft this buffer cannot place, shown for reference and left alone.
    Kept(&'a Action, String),
}

#[derive(Default)]
struct Layout<'a> {
    top: Vec<Note<'a>>,
    under: HashMap<usize, Vec<Note<'a>>>,
    end: Vec<Note<'a>>,
}

fn render(
    repo: &Repository,
    view: &View,
    patch: &Patch,
    threads: &[Thread],
    draft: &Local,
) -> (String, Base) {
    let line_index = patch.line_index();
    let file_index = patch.file_index();
    let mut moved = LineMap::new(repo, view.head);
    let mut layout = Layout::default();

    for t in threads {
        match &t.anchor {
            None => layout
                .top
                .push(Note::Thread(t, Some("conversation".into()))),
            Some(Anchor::File { path }) => match file_index.get(path.as_str()) {
                Some(&i) => layout
                    .under
                    .entry(i)
                    .or_default()
                    .push(Note::Thread(t, None)),
                None => layout.end.push(Note::Thread(t, Some(where_(t)))),
            },
            Some(Anchor::Line { path, end, .. }) => {
                let line = match t.commit.as_deref() {
                    None => Some(end.line),
                    Some(c) if c == view.head => Some(end.line),
                    Some(c) if end.side == Side::New => moved.line(c, path, end.line),
                    Some(_) => None,
                };
                match line.and_then(|l| line_index.get(&(path.as_str(), end.side, l))) {
                    Some(&i) => layout
                        .under
                        .entry(i)
                        .or_default()
                        .push(Note::Thread(t, None)),
                    None => layout.end.push(Note::Thread(t, Some(where_(t)))),
                }
            }
        }
    }

    let thread_ids: HashSet<String> = threads.iter().map(|t| bare(&t.id)).collect();
    let mut replies: HashMap<String, Vec<&Action>> = HashMap::new();
    let mut kept = Vec::new();
    let mut summary = None;
    for action in &draft.actions {
        match action {
            Action::Reply { thread, .. } | Action::Resolve { thread, .. } => {
                if thread_ids.contains(thread.as_str()) {
                    replies.entry(thread.to_string()).or_default().push(action);
                } else {
                    kept.push(Note::Kept(
                        action,
                        format!("its thread {thread} is not cached"),
                    ));
                }
            }
            Action::Comment {
                file,
                line,
                side,
                commit,
                ..
            } => {
                if commit.as_deref().is_some_and(|c| c != view.head) {
                    let on = commit.as_deref().map(short).unwrap_or_default();
                    kept.push(Note::Kept(action, format!("written on {on}")));
                    continue;
                }
                let at = match (file, line) {
                    (None, _) => {
                        layout.top.push(Note::Draft(action));
                        continue;
                    }
                    (Some(file), None) => file_index.get(file.as_str()),
                    (Some(file), Some(line)) => {
                        line_index.get(&(file.as_str(), side.unwrap_or(Side::New), *line))
                    }
                };
                match at {
                    Some(&i) => layout.under.entry(i).or_default().push(Note::Draft(action)),
                    None => kept.push(Note::Kept(action, "not on this diff".into())),
                }
            }
            Action::Summary { id, body } => {
                summary = id.as_ref().map(|id| ShownSummary {
                    id: id.clone(),
                    body: body.clone(),
                })
            }
            Action::Drop { .. } => {}
        }
    }
    kept.append(&mut layout.end);
    layout.end = kept;

    let mut base = Base {
        head: view.head.to_owned(),
        verdict: draft.verdict,
        summary,
        drafts: Vec::new(),
    };
    let mut out = String::new();
    if let Some(v) = draft.verdict {
        out.push_str(&format!("/{}\n", v.display_str()));
    }
    if let Some(s) = &base.summary {
        out.push_str(s.body.trim_end());
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&help(view));
    let notes = |out: &mut String, list: &[Note], base: &mut Base| {
        for note in list {
            push_note(out, note, &replies, base);
        }
    };
    notes(&mut out, &layout.top, &mut base);
    for (i, line) in patch.lines.iter().enumerate() {
        out.push_str("> ");
        out.push_str(&line.text);
        out.push('\n');
        if let Some(list) = layout.under.get(&i) {
            notes(&mut out, list, &mut base);
        }
    }
    if !layout.end.is_empty() {
        out.push_str(">> Outside this diff\n");
        notes(&mut out, &layout.end, &mut base);
    }
    (out, base)
}

fn help(view: &View) -> String {
    format!(
        "\
>> wits review edit · MR {id} · {title}
>> review point {fork}..{head}
>>   Write on unquoted lines. Lines quoted with \">\" are the patch and the
>>   discussion; they are never sent, and only whole files may be deleted.
>>   - Above the first patch line: the review summary. \"/approve\",
>>     \"/request-changes\" or \"/comment\" on a line of its own sets the verdict.
>>   - Under a patch line: a comment on that line; under a file's header, on
>>     the file. \"/span\" alone before a patch line starts a range that the
>>     next comment in its hunk covers.
>>   - Under a thread: a reply. \"/resolve\" or \"/unresolve\" alone resolves
>>     it or reopens it.
>>   - Under a \">> draft\" line: that draft's text. Edit it, move it with its
>>     line to re-anchor it, or delete both to drop it.
>>   Save and quit to apply. A buffer that does not parse is kept for the next
>>   `wits review edit {id}`.
",
        id = view.id,
        title = view.title,
        fork = short(view.fork),
        head = short(view.head),
    )
}

fn push_note(
    out: &mut String,
    note: &Note,
    replies: &HashMap<String, Vec<&Action>>,
    base: &mut Base,
) {
    match note {
        Note::Thread(t, place) => {
            let id = bare(&t.id);
            let mut header = vec![format!(">> thread {id}")];
            header.extend(place.clone());
            header.push(if t.resolved { "resolved" } else { "unresolved" }.to_owned());
            if t.outdated {
                header.push("outdated".to_owned());
            }
            out.push_str(&header.join(" · "));
            out.push('\n');
            for c in &t.comments {
                let mut lines = c.body.lines();
                out.push_str(&format!(
                    ">>   {}: {}\n",
                    c.author,
                    lines.next().unwrap_or("")
                ));
                for line in lines {
                    out.push_str(&format!(">>     {line}\n"));
                }
            }
            for action in replies.get(&id).into_iter().flatten() {
                push_draft(out, action);
                base.drafts.push((*action).clone());
            }
        }
        Note::Draft(action) => {
            push_draft(out, action);
            base.drafts.push((*action).clone());
        }
        Note::Kept(action, why) => {
            let id = action.id().unwrap_or_default();
            out.push_str(&format!(
                ">>   kept draft {id} · {why}; `wits review draft` changes it\n"
            ));
            for line in editable_text(action).lines() {
                out.push_str(&format!(">>     {line}\n"));
            }
        }
    }
}

fn push_draft(out: &mut String, action: &Action) {
    let id = action.id().unwrap_or_default();
    let kind = match action {
        Action::Comment {
            start_line: Some(start),
            line: Some(end),
            ..
        } => format!("comment on lines {start}-{end}"),
        Action::Comment { .. } => "comment".to_owned(),
        Action::Reply { .. } => "reply".to_owned(),
        Action::Resolve { .. } => "resolve".to_owned(),
        Action::Summary { .. } | Action::Drop { .. } => unreachable!("never shown as a draft"),
    };
    out.push_str(&format!(">> draft {id} · {kind}\n"));
    out.push_str(editable_text(action).trim_end());
    out.push('\n');
}

/// A draft's text as the buffer shows it under its marker.
fn editable_text(action: &Action) -> String {
    match action {
        Action::Comment { body, .. }
        | Action::Reply { body, .. }
        | Action::Summary { body, .. } => body.clone(),
        Action::Resolve { resolved: true, .. } => "/resolve".to_owned(),
        Action::Resolve {
            resolved: false, ..
        } => "/unresolve".to_owned(),
        Action::Drop { .. } => String::new(),
    }
}

/// A thread id without its `remote:` prefix.
fn bare(id: &str) -> String {
    ThreadId::from(id).to_string()
}

/// Where a thread sits, for one shown away from its line.
fn where_(t: &Thread) -> String {
    let on = t
        .commit
        .as_deref()
        .map(|c| format!(" on {}", short(c)))
        .unwrap_or_default();
    match &t.anchor {
        Some(Anchor::Line { path, end, .. }) => {
            format!("{path}:{} ({}){on}", end.line, end.side.as_str())
        }
        Some(Anchor::File { path }) => format!("{path}{on}"),
        None => "conversation".to_owned(),
    }
}

/// Where a new-side line of an earlier review point sits at the head, read off
/// one diff per file and commit.
struct LineMap<'a> {
    repo: &'a Repository,
    head: &'a str,
    hunks: HashMap<(String, String), Option<Vec<lines::Hunk>>>,
}

impl<'a> LineMap<'a> {
    fn new(repo: &'a Repository, head: &'a str) -> Self {
        LineMap {
            repo,
            head,
            hunks: HashMap::new(),
        }
    }

    fn line(&mut self, commit: &str, path: &str, line: u32) -> Option<u32> {
        let (repo, head) = (self.repo, self.head);
        let hunks = self
            .hunks
            .entry((commit.to_owned(), path.to_owned()))
            .or_insert_with(|| {
                repo.rev_parse(commit)?;
                Some(
                    repo.diff_hunks(commit, head, path, None)
                        .map(|patch| lines::hunks(&patch))
                        .unwrap_or_default(),
                )
            });
        lines::new_line_of(hunks.as_ref()?, line)
    }
}

// ---------------------------------------------------------------------------
// Reading a buffer back.
// ---------------------------------------------------------------------------

/// What a buffer says, before it is measured against what it showed.
#[derive(Debug, Default)]
struct Written {
    verdict: Option<Verdict>,
    summary: String,
    /// New comments, replies and resolves.
    added: Vec<Action>,
    /// The text under each draft marker.
    drafts: Vec<DraftText>,
}

#[derive(Debug)]
struct DraftText {
    id: String,
    /// Where the marker sits: `None` above the patch.
    place: Option<Place>,
    text: String,
    line: usize,
}

/// Where a comment sits: a file, and a line of it unless it is on the whole file.
#[derive(Debug, Clone, PartialEq)]
struct Place {
    file: String,
    end: Option<(u32, Side)>,
    start: Option<(u32, Side)>,
}

/// The reading state between two quoted lines.
#[derive(Default)]
struct Reader {
    /// The next rendered patch line a quoted line must be.
    next: usize,
    /// The last patch line read, with a line of its own (never a marker).
    pos: Option<usize>,
    seen_patch: bool,
    thread: Option<ThreadId>,
    draft: Option<(String, Option<usize>, usize)>,
    /// A `/span` waiting for its first line, and the line it was on.
    span_wanted: Option<usize>,
    /// A span's first patch line, and the buffer line of its `/span`.
    span: Option<(usize, usize)>,
    /// The first quoted line that was not the patch's next line.
    stray: Option<usize>,
    block: Vec<String>,
    block_line: usize,
    summary: Vec<String>,
    out: Written,
}

fn parse(text: &str, patch: &Patch) -> Result<Written> {
    let mut r = Reader::default();
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if let Some(rest) = line.strip_prefix(">>") {
            r.flush(patch)?;
            if let Some(id) = rest.strip_prefix(" thread ") {
                r.thread = Some(token(id).into());
                r.draft = None;
            } else if let Some(id) = rest.strip_prefix(" draft ") {
                r.draft = Some((token(id).to_owned(), r.pos, n));
            } else if !rest.starts_with("  ") {
                r.thread = None;
                r.draft = None;
                if r.seen_patch {
                    r.pos = None;
                }
            }
        } else if let Some(k) = line.strip_prefix('>').and_then(|q| r.matches(patch, q)) {
            r.flush(patch)?;
            r.read_patch_line(patch, k, n)?;
        } else {
            if line.starts_with('>') && r.stray.is_none() {
                r.stray = Some(n);
            }
            if r.block.is_empty() {
                r.block_line = n;
            }
            r.block.push(line.to_owned());
        }
    }
    r.flush(patch)?;
    if r.next < patch.lines.len() && !starts_file(patch, r.next) {
        let at = r
            .stray
            .map_or("the end".to_owned(), |n| format!("line {n}"));
        anyhow::bail!(
            "the quoted patch stops matching at {at}, where `{}` was due: quoted lines \
             cannot be edited, only whole files deleted",
            patch.lines[r.next].text
        );
    }
    if let Some((_, n)) = r.span {
        anyhow::bail!("line {n}: no comment follows this /span in its hunk");
    }
    if let Some(n) = r.span_wanted {
        anyhow::bail!("line {n}: no patch line follows this /span");
    }
    let mut summary = Vec::new();
    for block in &r.summary {
        let mut kept = Vec::new();
        for line in block.lines() {
            match line.trim() {
                "/approve" => r.out.verdict = Some(Verdict::Approve),
                "/request-changes" => r.out.verdict = Some(Verdict::RequestChanges),
                "/comment" => r.out.verdict = Some(Verdict::Comment),
                _ => kept.push(line),
            }
        }
        let text = tidy(kept.into_iter());
        if !text.is_empty() {
            summary.push(text);
        }
    }
    r.out.summary = summary.join("\n\n");
    Ok(r.out)
}

impl Reader {
    /// The rendered line that quoted text `q` is, if it is one: the patch's
    /// next line, or the header of a later file when whole files were deleted.
    fn matches(&self, patch: &Patch, q: &str) -> Option<usize> {
        let q = q.strip_prefix(' ').unwrap_or(q).trim_end();
        let same = |k: usize| patch.lines[k].text.trim_end() == q;
        if self.next < patch.lines.len() && same(self.next) {
            return Some(self.next);
        }
        if q.starts_with("diff --git ") && starts_file(patch, self.next) {
            return (self.next..patch.lines.len()).find(|&k| starts_file(patch, k) && same(k));
        }
        None
    }

    fn read_patch_line(&mut self, patch: &Patch, k: usize, n: usize) -> Result<()> {
        self.next = k + 1;
        self.seen_patch = true;
        self.thread = None;
        self.draft = None;
        let at = patch.lines[k].at;
        if at == At::Marker {
            return Ok(());
        }
        if let Some((s, line)) = self.span {
            if !same_hunk(patch, s, k) {
                anyhow::bail!("line {line}: no comment follows this /span in its hunk");
            }
        }
        if let Some(line) = self.span_wanted.take() {
            anyhow::ensure!(
                matches!(at, At::Line { .. }),
                "line {line}: a /span goes right before a line of a hunk, not a header (line {n})"
            );
            self.span = Some((k, line));
        }
        self.pos = Some(k);
        Ok(())
    }

    /// Take the text gathered since the last quoted line.
    fn flush(&mut self, patch: &Patch) -> Result<()> {
        let text = tidy(self.block.iter().map(String::as_str));
        let n = self.block_line;
        self.block.clear();
        if let Some((id, pos, line)) = &self.draft {
            match self.out.drafts.iter_mut().find(|d| d.line == *line) {
                Some(d) if d.text.is_empty() => d.text = text,
                Some(d) if !text.is_empty() => d.text = format!("{}\n{text}", d.text),
                Some(_) => {}
                None => self.out.drafts.push(DraftText {
                    id: id.clone(),
                    place: pos.map(|p| place(patch, p, None)),
                    text,
                    line: *line,
                }),
            }
            return Ok(());
        }
        if text.is_empty() {
            return Ok(());
        }
        if text == "/span" {
            anyhow::ensure!(
                self.span_wanted.is_none() && self.span.is_none(),
                "line {n}: a /span is already open"
            );
            self.span_wanted = Some(n);
            return Ok(());
        }
        anyhow::ensure!(
            !text.lines().any(|l| l.trim() == "/span"),
            "line {n}: /span goes on a line of its own, before the first line of its range"
        );
        if let Some(thread) = &self.thread {
            let mut body = Vec::new();
            let mut resolved = None;
            for line in text.lines() {
                match line.trim() {
                    "/resolve" => resolved = Some(true),
                    "/unresolve" => resolved = Some(false),
                    _ => body.push(line),
                }
            }
            let body = tidy(body.into_iter());
            if !body.is_empty() {
                self.out.added.push(Action::Reply {
                    id: None,
                    thread: thread.clone(),
                    body,
                });
            }
            if let Some(resolved) = resolved {
                self.out.added.push(Action::Resolve {
                    id: None,
                    thread: thread.clone(),
                    resolved,
                });
            }
            return Ok(());
        }
        if let Some(k) = self.pos {
            let start = self.span.take().map(|(s, _)| s);
            let p = place(patch, k, start);
            self.out.added.push(comment(None, Some(&p), text));
            return Ok(());
        }
        anyhow::ensure!(
            !self.seen_patch,
            "line {n}: this text is under no patch line, thread or draft"
        );
        self.summary.push(text);
        Ok(())
    }
}

fn token(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("")
}

fn starts_file(patch: &Patch, k: usize) -> bool {
    patch
        .lines
        .get(k)
        .is_some_and(|l| l.text.starts_with("diff --git "))
}

fn same_hunk(patch: &Patch, a: usize, b: usize) -> bool {
    match (patch.lines[a].at, patch.lines[b].at) {
        (At::Line { hunk: x, .. }, At::Line { hunk: y, .. }) => x == y,
        _ => false,
    }
}

/// The place of rendered line `k`, spanning from line `start` when given.
fn place(patch: &Patch, k: usize, start: Option<usize>) -> Place {
    let line_of = |k: usize| match patch.lines[k].at {
        At::Line { side, line, .. } => Some((line, side)),
        _ => None,
    };
    let file = match patch.lines[k].at {
        At::Header { file } | At::Line { file, .. } => patch.path(file).to_owned(),
        At::Marker => unreachable!("a marker is never a position"),
    };
    Place {
        file,
        end: line_of(k),
        start: start.filter(|&s| s != k).and_then(line_of),
    }
}

fn comment(id: Option<String>, place: Option<&Place>, body: String) -> Action {
    Action::Comment {
        id,
        file: place.map(|p| p.file.clone()),
        line: place.and_then(|p| p.end).map(|e| e.0),
        side: place.and_then(|p| p.end).map(|e| e.1),
        start_line: place.and_then(|p| p.start).map(|s| s.0),
        start_side: place.and_then(|p| p.start).map(|s| s.1),
        body,
        commit: None,
    }
}

/// Text without its leading and trailing blank lines.
fn tidy<'a>(lines: impl Iterator<Item = &'a str>) -> String {
    let lines: Vec<&str> = lines.collect();
    let blank = |l: &&str| l.trim().is_empty();
    let start = lines.iter().position(|l| !blank(l)).unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|l| !blank(l))
        .map_or(start, |e| e + 1);
    lines[start..end].join("\n")
}

/// The draft actions that turn what was shown into what was written, and the
/// verdict when it changed.
fn changes(
    base: &Base,
    written: Written,
    head: &str,
) -> Result<(Vec<Action>, Option<Option<Verdict>>)> {
    let mut out = Vec::new();
    match (&base.summary, written.summary.is_empty()) {
        (Some(s), true) => out.push(Action::Drop { id: s.id.clone() }),
        (Some(s), false) if tidy(s.body.lines()) != written.summary => out.push(Action::Summary {
            id: Some(s.id.clone()),
            body: written.summary,
        }),
        (None, false) => out.push(Action::Summary {
            id: None,
            body: written.summary,
        }),
        _ => {}
    }

    let mut seen = HashSet::new();
    for d in &written.drafts {
        let shown = base
            .drafts
            .iter()
            .find(|a| a.id() == Some(d.id.as_str()))
            .with_context(|| {
                format!(
                    "line {}: draft {} is not one this buffer showed",
                    d.line, d.id
                )
            })?;
        anyhow::ensure!(
            seen.insert(d.id.as_str()),
            "line {}: draft {} appears twice",
            d.line,
            d.id
        );
        out.extend(rewrite(shown, d, head)?);
    }
    for shown in &base.drafts {
        let id = shown.id().expect("shown drafts have ids");
        if !seen.contains(id) {
            out.push(Action::Drop { id: id.to_owned() });
        }
    }
    out.extend(written.added);
    let verdict = (written.verdict != base.verdict).then_some(written.verdict);
    Ok((out, verdict))
}

/// The action that makes draft `shown` read as `d`; `None` when it already does.
fn rewrite(shown: &Action, d: &DraftText, head: &str) -> Result<Option<Action>> {
    let id = Some(d.id.clone());
    if d.text.is_empty() {
        return Ok(Some(Action::Drop { id: d.id.clone() }));
    }
    let same_text = |body: &str| tidy(body.lines()) == d.text;
    Ok(match shown {
        Action::Comment {
            file,
            line,
            side,
            start_line,
            start_side,
            body,
            ..
        } => {
            let was = file.as_ref().map(|file| Place {
                file: file.clone(),
                end: line.map(|l| (l, side.unwrap_or(Side::New))),
                start: start_line.map(|s| (s, start_side.or(*side).unwrap_or(Side::New))),
            });
            let stayed = match (&was, &d.place) {
                (Some(was), Some(now)) => was.file == now.file && was.end == now.end,
                (None, None) => true,
                _ => false,
            };
            if stayed && same_text(body) {
                None
            } else {
                let place = if stayed { was } else { d.place.clone() };
                let mut action = comment(id, place.as_ref(), d.text.clone());
                if let Action::Comment { commit, .. } = &mut action {
                    *commit = Some(head.to_owned());
                }
                Some(action)
            }
        }
        Action::Reply { thread, body, .. } => (!same_text(body)).then(|| Action::Reply {
            id,
            thread: thread.clone(),
            body: d.text.clone(),
        }),
        Action::Resolve {
            thread, resolved, ..
        } => {
            let now = match d.text.as_str() {
                "/resolve" => true,
                "/unresolve" => false,
                _ => anyhow::bail!(
                    "line {}: draft {} resolves a thread, so it holds /resolve or /unresolve",
                    d.line,
                    d.id
                ),
            };
            (now != *resolved).then(|| Action::Resolve {
                id,
                thread: thread.clone(),
                resolved: now,
            })
        }
        Action::Summary { .. } | Action::Drop { .. } => {
            anyhow::bail!("line {}: draft {} cannot be edited here", d.line, d.id)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::review::model::Comment;
    use wits_util::forge::LineRef;

    const HEAD: &str = "headheadheadhead";

    fn patch_text() -> String {
        [
            "diff --git a/src/a.c b/src/a.c",
            "index 1111111..2222222 100644",
            "--- a/src/a.c",
            "+++ b/src/a.c",
            "@@ -1,4 +1,4 @@ int main",
            " one",
            "-two",
            "+TWO",
            " three",
            " ",
            "@@ -10,2 +10,3 @@",
            " ten",
            "+ten and a half",
            " eleven",
            "diff --git a/old name.c b/new name.c",
            "similarity index 90%",
            "rename from old name.c",
            "rename to new name.c",
            "--- a/old name.c",
            "+++ b/new name.c",
            "@@ -1 +1 @@",
            "-x",
            "+y",
            "diff --git a/gone.c b/gone.c",
            "deleted file mode 100644",
            "--- a/gone.c",
            "+++ /dev/null",
            "@@ -1 +0,0 @@",
            "-bye",
            "\\ No newline at end of file",
            "",
        ]
        .join("\n")
    }

    fn view() -> View<'static> {
        View {
            id: "7",
            title: "#7 Fix things",
            fork: "forkforkforkfork",
            head: HEAD,
        }
    }

    fn thread(id: &str, line: u32) -> Thread {
        Thread {
            id: format!("remote:{id}"),
            origin: "remote".into(),
            resolved: false,
            outdated: false,
            anchor: Some(Anchor::Line {
                path: "src/a.c".into(),
                old_path: None,
                end: LineRef {
                    line,
                    side: Side::New,
                    old_line: None,
                },
                start: None,
            }),
            commit: Some(HEAD.into()),
            comments: vec![Comment {
                id: "remote:1".into(),
                author: "bob".into(),
                origin: "remote".into(),
                body: "why upper case?\nit shouts".into(),
                created_at: String::new(),
                state: "published".into(),
            }],
        }
    }

    fn draft_comment(id: &str, line: u32, body: &str, commit: &str) -> Action {
        Action::Comment {
            id: Some(id.into()),
            file: Some("src/a.c".into()),
            line: Some(line),
            side: Some(Side::New),
            start_line: None,
            start_side: None,
            body: body.into(),
            commit: Some(commit.into()),
        }
    }

    fn rendered(threads: &[Thread], draft: &Local) -> (String, Base, Patch) {
        let patch = Patch::parse(&patch_text());
        let repo = Repository::new(std::path::Path::new("/nonexistent"));
        let (text, base) = render(&repo, &view(), &patch, threads, draft);
        (text, base, patch)
    }

    /// `text` with `insert` put after the line that is exactly `after`.
    fn under(text: &str, after: &str, insert: &str) -> String {
        let at = text
            .find(&format!("{after}\n"))
            .unwrap_or_else(|| panic!("no line {after:?} in:\n{text}"));
        let at = at + after.len() + 1;
        format!("{}{insert}{}", &text[..at], &text[at..])
    }

    fn read(text: &str, base: &Base, patch: &Patch) -> (Vec<Action>, Option<Option<Verdict>>) {
        changes(base, parse(text, patch).unwrap(), HEAD).unwrap()
    }

    #[test]
    fn a_patch_knows_each_line_and_each_file_by_its_comment_path() {
        let patch = Patch::parse(&patch_text());
        assert_eq!(patch.files, ["src/a.c", "new name.c", "gone.c"]);
        let at = |text: &str| {
            patch
                .lines
                .iter()
                .find(|l| l.text == text)
                .map(|l| l.at)
                .unwrap()
        };
        let line = |side, line, old| At::Line {
            file: 0,
            hunk: 0,
            side,
            line,
            old,
        };
        let strip_hunk = |a: At| match a {
            At::Line {
                file,
                side,
                line,
                old,
                ..
            } => At::Line {
                file,
                hunk: 0,
                side,
                line,
                old,
            },
            other => other,
        };
        assert_eq!(strip_hunk(at("-two")), line(Side::Old, 2, None));
        assert_eq!(strip_hunk(at("+TWO")), line(Side::New, 2, None));
        assert_eq!(strip_hunk(at(" three")), line(Side::New, 3, Some(3)));
        assert_eq!(strip_hunk(at(" ")), line(Side::New, 4, Some(4)));
        assert_eq!(strip_hunk(at(" eleven")), line(Side::New, 12, Some(11)));
        assert_eq!(at("\\ No newline at end of file"), At::Marker);
    }

    #[test]
    fn an_unusual_path_is_read_whichever_way_git_writes_it() {
        assert_eq!(
            header_path(r#""a/sp\303\244ce.c" "b/sp\303\244ce.c""#),
            "späce.c"
        );
        assert_eq!(header_path("a/x b/y.c b/x b/y.c"), "x b/y.c");
        assert_eq!(field_path(r#""b/tab\there""#, "b/"), "tab\there");
    }

    #[test]
    fn a_buffer_read_back_untouched_changes_nothing() {
        let draft = Local {
            verdict: Some(Verdict::RequestChanges),
            actions: vec![
                Action::Summary {
                    id: Some("s1".into()),
                    body: "Overall fine.\n\nTwo nits.".into(),
                },
                draft_comment("c1", 3, "this line", HEAD),
                draft_comment("c2", 3, "an earlier look", "oldoldoldold"),
                Action::Reply {
                    id: Some("r1".into()),
                    thread: "9".into(),
                    body: "it is a constant".into(),
                },
                Action::Resolve {
                    id: Some("v1".into()),
                    thread: "9".into(),
                    resolved: true,
                },
            ],
            ..Default::default()
        };
        let (text, base, patch) = rendered(&[thread("9", 2)], &draft);
        assert_eq!(read(&text, &base, &patch), (vec![], None), "{text}");
        assert_eq!(base.drafts.len(), 3, "c2 is kept, not editable");

        // An editor that strips trailing whitespace changes nothing either.
        let stripped: String = text
            .lines()
            .map(|l| format!("{}\n", l.trim_end()))
            .collect();
        assert_eq!(read(&stripped, &base, &patch), (vec![], None));
    }

    #[test]
    fn text_under_each_kind_of_line_becomes_its_action() {
        let (text, base, patch) = rendered(&[thread("9", 2)], &Local::default());
        let text = format!("/approve\nLooks good.\n{text}");
        let text = under(&text, "> +++ b/src/a.c", "on the file\n");
        let text = under(&text, "> -two", "on the old line\n");
        let text = under(&text, "> +TWO", "on the new line\n");
        let text = under(&text, ">>     it shouts", "agreed\n/resolve\n");
        let text = under(&text, ">  three", "> as bob said\nmore\n");
        let (actions, verdict) = read(&text, &base, &patch);
        assert_eq!(verdict, Some(Some(Verdict::Approve)));
        let at = |file: &str, line: Option<u32>, side: Option<Side>, body: &str| Action::Comment {
            id: None,
            file: Some(file.into()),
            line,
            side,
            start_line: None,
            start_side: None,
            body: body.into(),
            commit: None,
        };
        assert_eq!(
            actions,
            [
                Action::Summary {
                    id: None,
                    body: "Looks good.".into()
                },
                at("src/a.c", None, None, "on the file"),
                at("src/a.c", Some(2), Some(Side::Old), "on the old line"),
                at("src/a.c", Some(2), Some(Side::New), "on the new line"),
                Action::Reply {
                    id: None,
                    thread: "9".into(),
                    body: "agreed".into()
                },
                Action::Resolve {
                    id: None,
                    thread: "9".into(),
                    resolved: true
                },
                at("src/a.c", Some(3), Some(Side::New), "> as bob said\nmore"),
            ]
        );
    }

    #[test]
    fn a_span_runs_from_the_line_after_it_to_the_comment() {
        let (text, base, patch) = rendered(&[], &Local::default());
        let text = under(&text, ">  one", "/span\n");
        let text = under(&text, "> +TWO", "both of these\n");
        let (actions, _) = read(&text, &base, &patch);
        let Action::Comment {
            line,
            side,
            start_line,
            start_side,
            ..
        } = &actions[0]
        else {
            panic!("{actions:?}")
        };
        assert_eq!((*start_line, *start_side), (Some(2), Some(Side::Old)));
        assert_eq!((*line, *side), (Some(2), Some(Side::New)));

        let open = under(&text, ">  ten", "/span\n");
        let err = parse(&open, &patch).unwrap_err().to_string();
        assert!(err.contains("no comment follows this /span"), "{err}");
    }

    #[test]
    fn whole_files_may_go_but_no_quoted_line_may_change() {
        let (text, base, patch) = rendered(&[], &Local::default());
        let start = text.find("> diff --git a/old name.c").unwrap();
        let end = text.find("> diff --git a/gone.c").unwrap();
        let cut = format!("{}{}", &text[..start], &text[end..]);
        let cut = under(&cut, "> -bye", "farewell\n");
        let (actions, _) = read(&cut, &base, &patch);
        assert!(
            matches!(&actions[..], [Action::Comment { file: Some(f), line: Some(1), side: Some(Side::Old), .. }] if f == "gone.c"),
            "{actions:?}"
        );

        let edited = text.replace("> +TWO\n", "> +TW0\n");
        let err = parse(&edited, &patch).unwrap_err().to_string();
        assert!(err.contains("stops matching at line"), "{err}");
        assert!(err.contains("`+TWO` was due"), "{err}");
    }

    #[test]
    fn text_outside_the_diff_belongs_nowhere() {
        let draft = Local {
            actions: vec![draft_comment("c2", 3, "earlier", "oldoldoldold")],
            ..Default::default()
        };
        let (text, _, patch) = rendered(&[], &draft);
        let text = format!("{text}stray words\n");
        let err = parse(&text, &patch).unwrap_err().to_string();
        assert!(
            err.contains("under no patch line, thread or draft"),
            "{err}"
        );
    }

    #[test]
    fn a_draft_is_rewritten_moved_or_dropped_by_its_text() {
        let draft = Local {
            actions: vec![
                draft_comment("c1", 3, "first", HEAD),
                draft_comment("c2", 12, "second", HEAD),
                draft_comment("c3", 12, "third", HEAD),
            ],
            ..Default::default()
        };
        let (text, base, patch) = rendered(&[], &draft);
        // c1 reworded, c2's marker and text deleted, c3 moved to the added line.
        let text = text.replace("first\n", "first, reworded\n");
        let text = text.replace(">> draft c2 · comment\nsecond\n", "");
        let text = text.replace(">> draft c3 · comment\nthird\n", "");
        let text = under(&text, "> +ten and a half", ">> draft c3 · comment\nthird\n");
        let (actions, _) = read(&text, &base, &patch);
        let comment = |id: &str, line, body: &str| Action::Comment {
            id: Some(id.into()),
            file: Some("src/a.c".into()),
            line: Some(line),
            side: Some(Side::New),
            start_line: None,
            start_side: None,
            body: body.into(),
            commit: Some(HEAD.into()),
        };
        assert_eq!(
            actions,
            [
                comment("c1", 3, "first, reworded"),
                comment("c3", 11, "third"),
                Action::Drop { id: "c2".into() },
            ]
        );

        // Emptying a draft's text drops it too.
        let (text, base, patch) = rendered(&[], &draft);
        let text = text.replace("first\n", "");
        let (actions, _) = read(&text, &base, &patch);
        assert_eq!(actions, [Action::Drop { id: "c1".into() }]);
    }

    #[test]
    fn a_threads_draft_follows_it_and_the_summary_can_go() {
        let draft = Local {
            actions: vec![
                Action::Summary {
                    id: Some("s1".into()),
                    body: "Overall fine.".into(),
                },
                Action::Resolve {
                    id: Some("v1".into()),
                    thread: "9".into(),
                    resolved: true,
                },
            ],
            ..Default::default()
        };
        let (text, base, patch) = rendered(&[thread("9", 2)], &draft);
        let thread_at = text.find(">> thread 9").unwrap();
        assert!(thread_at > text.find("> +TWO").unwrap(), "under its line");
        assert!(text[thread_at..].starts_with(">> thread 9 · unresolved\n>>   bob: why upper case?\n>>     it shouts\n>> draft v1 · resolve\n/resolve\n"));

        let text = text.replace("Overall fine.\n", "");
        let text = text.replace(
            ">> draft v1 · resolve\n/resolve\n",
            ">> draft v1 · resolve\n/unresolve\n",
        );
        let (actions, _) = read(&text, &base, &patch);
        assert_eq!(
            actions,
            [
                Action::Drop { id: "s1".into() },
                Action::Resolve {
                    id: Some("v1".into()),
                    thread: "9".into(),
                    resolved: false
                },
            ]
        );
    }
}
