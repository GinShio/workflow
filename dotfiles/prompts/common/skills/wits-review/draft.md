# Drafting into `local.json`

The disclosed write reference for [`wits-review`](SKILL.md). `wits review draft <mr> -` reads a JSON batch on stdin and appends its actions to the MR's `local.json`. Each finding is a `comment`; the overall note is one `summary`.

```json
{
  "schema": 1,
  "verdict": "comment",
  "actions": [
    { "action": "summary", "body": "<overall note>\n\n<trailer>" },
    { "action": "comment", "file": "src/lock.c", "line": 42, "side": "new",
      "body": "<the finding, with a [[path:line]] reference where it helps>\n\n<trailer>" }
  ]
}
```

Always set the top-level `verdict` to `comment` — never `approve` or `request-changes`, which are the author's to set.

## Actions

- `comment` — a finding (opens a new thread).
- `summary` — the one overall note; the last surviving summary is the review's summary.
- `reply` — add to an existing thread; set `thread` to its id (the bare forge id `show` prints).
- `drop` — withdraw a live action; set `id` to the one you're removing.

## Where a comment anchors

Placement is inferred from which fields are present:

| Fields | Placement |
|---|---|
| `file` + `line` | a line comment |
| `file` only | a file-level comment |
| neither | an MR-level note |

`side` is `new` for an added or context line, `old` for a removed one (default `new`). For a span, add `start_line` (with `start_side`) as the start; a differing `start_side` makes a cross-side span. Take line numbers from the patch or the file in your worktree. Leave `commit` unset — `draft` stamps the current snapshot at ingest.

## Reference code — `[[path:line]]`

A `[[path:line]]` token in any body becomes a forge permalink on submit, so prefer it over pasting a path or snippet when you want the author to jump somewhere. Grammar: `path` (repo-relative), optional `:line` or `:start-end`, optional `@ref` to pin a commit/branch/tag (default: the reviewed head). Examples: `[[src/y.c:20]]`, `[[src/y.c:20-30]]`, `[[src/y.c]]`, `[[src/y.c:20@main]]`.

## The provenance trailer

Every `comment`, `reply`, and `summary` body ends with exactly one trailer line:

```markdown
**Generated-by:** <tool> (<model>) <!-- this comment is submitted by wits-review -->
```

`<tool>` and `<model>` are the coding tool and active model, filled in by the adapter; the HTML comment renders invisibly on both forges.

## Revise or withdraw

The draft is append-only, keyed by `id` (`wits:<uuid>`, assigned when you omit it): a later action with the same `id` replaces the earlier one, and a `drop` removes it. To change something on a re-review, read the live actions with `wits review draft <mr> --json`, then append by id instead of duplicating.
