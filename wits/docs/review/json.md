# `wits review` — the JSON contract

This is the interface between `wits review` and an editor (or any front-end).

- **Read** through `--json`: `show`, `diff`, and `draft` emit versioned JSON on
  stdout.
- **Write** by editing `local.json` — the draft file whose schema is defined
  [below](#localjson---the-write-contract). A front-end writes this file; there
  are no authoring commands.

Every payload carries an integer `schema` (currently `1`). A reader that meets a
schema it doesn't know should refuse rather than guess.

## Shared conventions

| Concept | Values | Meaning |
|---|---|---|
| Remote id prefix | `remote:<forge-id>` | A forge-owned thread/comment id in the read view. |
| `side` | `new` / `old` | Post-image (added/context) / pre-image (deleted line). |
| `state` (MR) | `open` / `merged` / `closed` | The MR's lifecycle. Draft-ness is the separate `draft` bool, not folded in. |
| `verdict` | `approve` / `request-changes` / `comment` | The reviewer's disposition. |
| Action id | `wits:<uuid>` or a client-owned string | The logical identity of one pending action. Later actions with the same id replace earlier ones. |
| `origin` (comment) | `remote` / `local` | On the forge / pending in your draft. |
| `state` (comment) | `published` / `pending` | On the forge / not yet submitted. |

Optional fields are omitted when absent, never emitted as `null`.

---

## `show <mr> --json` — one MR's review state

The detail view, with the pending draft folded into the remote discussion.

```json
{
  "schema": 1,
  "mr": { "...": "MrSummary, see below" },
  "snapshot": { "base_sha": "aaaa…", "head_sha": "9f8e…" },
  "snapshots": [ { "base_sha": "aaaa…", "start_sha": "aaaa…", "head_sha": "9f8e…" } ],
  "neighbors": { "position": 1, "prev_mr": "122", "next_mr": "124",
                 "nodes": ["121","122","123","124"] },
  "commits": [ { "sha": "9f8e…", "subject": "Fix the lock ordering" } ],
  "files": [ { "path": "src/x.c", "old_path": "src/old.c", "status": "R" } ],
  "threads": [ { "...": "Thread, see below" } ],
  "draft": { "verdict": "request-changes", "summary": "…", "pending": 2 }
}
```

### Top-level fields

| Field | Type | Meaning |
|---|---|---|
| `schema` | int | Payload version. |
| `mr` | object | MR metadata (table below). |
| `snapshot.base_sha` | string | The base SHA of the current reviewed diff. |
| `snapshot.head_sha` | string | The current head SHA under review. **Render your diff between these two.** |
| `snapshots` | array | The full snapshot history, oldest first: `{ base_sha, start_sha, head_sha }` (a diff version). Each is a fetched, pinned review point; browse an older one with `diff --snapshot <head_sha>`. Distinct from an ad-hoc diff *range*. (When the MR was last synced is tracked once on the MR, not per snapshot.) |
| `neighbors` | object | This MR's place in its stack (table below). |
| `commits` | array | Commits in `base..head`, oldest first: `{ sha, subject }`. |
| `files` | array | Files the MR touched: `{ path, old_path?, status }`. `status` is git's letter (`A`/`M`/`D`/`R`/`C`); `old_path` present on a rename/copy. |
| `threads` | array | Every discussion, remote + pending, merged (table below). |
| `draft` | object | The pending review after append-only compaction: `verdict?`, effective `summary?`, and `pending` — a count of live actions plus one if a verdict is set. |

### `mr` object

| Field | Type | Meaning |
|---|---|---|
| `id` | string | The MR number (the address you pass to commands). |
| `display` | string | Human form (`#123` / `!123`). |
| `state` | string | `open`/`merged`/`closed` (lifecycle only). |
| `draft` | bool | Whether the MR is a draft/WIP. |
| `title`, `author` | string | As on the forge. |
| `base` | string | Target branch (what it merges into). |
| `source` | string | Source branch (used to link a stack). |
| `head_sha` | string? | Current head; omitted if unknown. |
| `updated_at` | string | ISO-8601 last-update time. |
| `labels` | array | Label names. |
| `web_url` | string | The MR's forge URL. |

### `neighbors` object

| Field | Type | Meaning |
|---|---|---|
| `nodes` | array | The stack's MR ids, bottom→top. |
| `position` | int | This MR's index in `nodes`. |
| `prev_mr` | string? | The MR one step down (omitted at the bottom). |
| `next_mr` | string? | The MR one step up (omitted at the top). |

### `Thread` object

| Field | Type | Meaning |
|---|---|---|
| `id` | string | `remote:<id>` for a forge thread, or the action id for a pending local thread. |
| `origin` | string | `remote` / `local`. |
| `resolved` | bool | Resolved on the forge (reflects a pending resolve too). |
| `outdated` | bool | The anchored line has left the current diff. |
| `anchor` | object? | The code anchor (table below); **absent** for an MR-level conversation thread. |
| `commit` | string? | The snapshot SHA the anchor was written against (drives outdate); omitted when unknown. |
| `comments` | array | The thread's comments; a pending reply is appended with `origin: local`, `state: pending`. |

`anchor`, when present, is tagged on `kind`:

| `kind` | Fields | Meaning |
|---|---|---|
| `line` | `path`, `end` {`line`, `side`}, `start`? {`line`, `side`}, `old_path?` | A code line. `end` is the anchor line; `start`, when present, makes a multi-line span and may carry a different `side` (a cross-side span). |
| `file` | `path` | A whole changed file, no line. |

An **MR-level** conversation thread carries no `anchor` field at all. This is the
same `Anchor` type the tool speaks internally — there is no separate "placement"
shape.

`Comment` object: `id`, `author`, `origin`, `body`, `created_at?`, `state`.

### Filters

`--outdated`, `--resolved`, `--unread`, `--file <path>` narrow `threads`
(AND-combined). `--unread` keeps threads whose last comment is `remote`.

---

## `show --json` — the inbox

With no MR, an array of MRs. Each item is the `mr` object flattened at the top
level, plus a `review` block.

```json
{
  "schema": 1,
  "items": [
    { "id": "123", "display": "#123", "state": "open", "draft": false,
      "title": "Fix the lock ordering", "author": "alice",
      "base": "main", "source": "fix-lock", "head_sha": "9f8e…",
      "updated_at": "2026-07-01T12:00:00Z", "labels": ["bug"],
      "web_url": "https://…/123",
      "review": { "pending": 2 } }
  ]
}
```

| Field | Type | Meaning |
|---|---|---|
| (mr fields) | — | Same as the `mr` object above, flattened. |
| `review.pending` | int | Count of live unsubmitted actions in this MR's draft (plus one for a set verdict). |

Items are sorted by `updated_at`, newest first.

---

## `diff <mr> --json` — diff coordinates

```json
{ "schema": 1, "mr": "123", "range": "aaaa…..9f8e…",
  "base_sha": "aaaa…", "head_sha": "9f8e…",
  "commits": [ { "sha": "…", "subject": "…" } ],
  "files": [ { "path": "src/x.c", "status": "M" } ] }
```

| Field | Type | Meaning |
|---|---|---|
| `mr` | string | The MR number. |
| `range` | string | The resolved range (`all` expands to `base..head`). |
| `base_sha`, `head_sha` | string | The two SHAs to diff against. |
| `commits` | array | Commits in the range: `{ sha, subject }`. |
| `files` | array | Files touched in the range: `{ path, old_path?, status }`. |

The tool renders no diff — this gives the coordinates so the editor renders its
own against `base_sha`/`head_sha`.

---

## `draft <mr> --json` — the pending draft

`draft --json` prints the MR's `local.json` verbatim (see the write contract).

---

## `local.json` — the write contract

This is the file a front-end (or a human) edits to author a review. It is the
one place `wits review` reads authored intent from, so its shape is a public,
versioned contract.

**Writing it.** A front-end doesn't need the store path: it pipes a batch of the
same shape to `wits review draft <mr> -` (or a file), and the tool **appends** the
batch's actions (setting `verdict` if present) and validates as it writes. Any
incoming action without an `id` is assigned one before it is stored. A human can
edit the file directly instead — equivalent.

```json
{
  "schema": 1,
  "verdict": "request-changes",
  "actions": [
    { "action": "summary", "id": "wits:550e8400-e29b-41d4-a716-446655440000", "body": "A few blockers below." },
    { "action": "comment", "id": "wits:550e8400-e29b-41d4-a716-446655440001", "file": "src/x.c", "line": 42, "body": "Off-by-one.", "commit": "a1b2c3d4" },
    { "action": "comment", "id": "wits:550e8400-e29b-41d4-a716-446655440002", "file": "src/x.c", "line": 42, "start_line": 40, "side": "old", "start_side": "old", "body": "…", "commit": "a1b2c3d4" },
    { "action": "comment", "id": "wits:550e8400-e29b-41d4-a716-446655440003", "file": "src/x.c", "body": "File-level note.", "commit": "a1b2c3d4" },
    { "action": "comment", "id": "wits:550e8400-e29b-41d4-a716-446655440004", "body": "MR-level conversation note." },
    { "action": "reply", "id": "wits:550e8400-e29b-41d4-a716-446655440005", "thread": "9987", "body": "Done." },
    { "action": "resolve", "id": "wits:550e8400-e29b-41d4-a716-446655440006", "thread": "9987", "resolved": true },
    { "action": "drop", "id": "wits:550e8400-e29b-41d4-a716-446655440004" }
  ]
}
```

### Top level

| Field | Type | Required | Meaning |
|---|---|---|---|
| `schema` | int | yes | Contract version (`1`). |
| `verdict` | string | no | `approve` / `request-changes` / `comment`. Omit for comments-only. |
| `actions` | array | no | The ordered list of things to post. |

### `actions[]` — tagged on `action`

Every stored action has an `id`. A batch sent to `draft <mr> -` may omit it; the
tool fills in a `wits:<uuid>` id before appending to `local.json`. Clients that
want to modify an earlier action append a new action with the same `id`.

The draft is append-only but compacts by id: reading for `show`, `draft --dedup`,
and `submit` scans actions from top to bottom. A later non-`drop` action replaces
the previous live action with the same `id`. A `drop` removes the current live
action with that id and is not itself a live action. `drop` cannot delete another
`drop`.

**`summary`** — the review's overall body:

| Field | Type | Required | Meaning |
|---|---|---|---|
| `id` | string | yes once stored | Logical action id. |
| `body` | string | yes | The review summary body. If several summary actions survive compaction, the last one is submitted as the review summary. |

**`comment`** — a new thread. Placement is inferred from which fields are present:

| Fields present | Placement |
|---|---|
| `file` + `line` | a line comment |
| `file` only | a file-level comment |
| neither | an MR-level conversation comment |

| Field | Type | Required | Meaning |
|---|---|---|---|
| `id` | string | yes once stored | Logical action id. |
| `file` | string | no | Path of a changed file. |
| `line` | int | no | Line number on `side`. |
| `side` | string | no | `new` (default) or `old`. |
| `start_line` | int | no | First line of a multi-line span (with `line` as the end). |
| `start_side` | string | no | Side of `start_line`; defaults to `side` when absent. Set it (differently from `side`) to express a cross-side span — e.g. starting on an `old` line and ending on a `new` one. |
| `body` | string | yes | The comment text. |
| `commit` | string | no | Snapshot head SHA this comment's line anchors were written against. Set by `draft <mr> -` at ingest; a hand-editor may set it. `submit` resolves it to the snapshot's full `{base, start, head}` and anchors the comment to that version (the forge may mark it outdated). When unset, falls back to the current snapshot. |

**`reply`** — add to an existing thread.

| Field | Type | Required | Meaning |
|---|---|---|---|
| `id` | string | yes once stored | Logical action id. |
| `thread` | string | yes | The thread id (bare forge id, or the `remote:` form `show` prints). On GitLab this is the discussion id; on GitHub the GraphQL review-thread node id (`PRRT_…`). |
| `body` | string | yes | The reply text. |

**`resolve`** — set a thread's resolved state (supported on both forges).

| Field | Type | Required | Meaning |
|---|---|---|---|
| `id` | string | yes once stored | Logical action id. |
| `thread` | string | yes | The thread id. |
| `resolved` | bool | yes | `true` to resolve, `false` to unresolve. |

**`drop`** — remove a pending local action:

| Field | Type | Required | Meaning |
|---|---|---|---|
| `id` | string | yes | The id of the live action to remove. A drop is local-only and is never submitted to the forge. |

### How `submit` treats it

- **Compact + de-duplicate:** ids drive local compaction. Later actions with the
  same id replace earlier ones, and `drop` removes a live local action. `draft
  --dedup` writes this compacted form back to `local.json`; `submit` always
  applies the same compaction before posting.
- **Batching:** the whole review is handed to the forge as one batch, folded
  into as few notifications as the platform allows. On GitLab comments (line/
  file/conversation), replies, and the summary (a position-less draft note) ride
  one bodyless `bulk_publish`; the verdict is a separate released call
  (`approve`→`/approve`, `request-changes`→`/unapprove`, `comment`→no-op — no
  released API sets the `reviewed`/`requested_changes` state), and a bare resolve
  is a separate PUT. On GitHub the verdict + summary + line/file comments **and replies** are
  one review (replies join the pending review, as in the web UI); only an
  MR-level conversation comment is a separate notification, and resolves are
  separate but quiet. `submit` reports the real notification count.
- **Anchoring:** each comment carries its own `commit` — the snapshot head its
  line anchors were written against. `submit` resolves it against the snapshot
  history to the full `{base, start, head}` version and anchors the comment to it.
  On GitLab this is **per-comment** (each diff note targets its own version), so
  different actions in one draft can target different snapshots — true
  cross-snapshot drafting. On GitHub the whole review anchors to one review-level
  `commitOID` (the API takes one per review), so the batch anchors to the review's
  snapshot. Comments without a `commit` are stamped with the current snapshot at
  normalize time and anchor to the current head on both backends.
- **References:** a `[[path:line]]` token in any `body` is expanded to a forge
  permalink. Grammar: `path` (repo-relative), optional `:line` or `:start-end`,
  optional `@ref` to pin a commit/branch/tag (default: the reviewed head for that
  comment, i.e. its own `commit` when set). Examples: `[[src/y.c:20]]`,
  `[[src/y.c:20-30]]`, `[[src/y.c]]`, `[[src/y.c:20@main]]`. Unparseable tokens are
  left as written.
- **Failure / retry:** reconciliation is per submitted action — whatever landed is cleared,
  whatever failed stays in the draft. If an attempt creates forge-side state it
  can't publish (GitLab draft notes, a GitHub pending review), it records those
  ids and the *next* submit deletes them first (deferred, idempotent cleanup that
  only ever touches ids the tool itself created), so a retry duplicates nothing.
  An empty draft post-`submit` triggers a re-fetch so your comments return as
  remote threads.
