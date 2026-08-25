# Drafting into `local.json`

The disclosed write reference for [`wits-review`](SKILL.md). `wits review draft <mr> -` validates a JSON batch, stamps unstamped comments with the current snapshot, assigns missing action ids, and appends the actions to `local.json`. It stores body text verbatim.

Represent the `review-protocol` summary with one live `summary` action and each finding with one `comment`. LGTM is the summary with no finding comments.

```json
{
  "schema": 1,
  "verdict": "comment",
  "actions": [
    { "action": "summary", "body": "<overall note>\n\n<provenance trailer>" },
    { "action": "comment", "file": "src/lock.c", "line": 42, "side": "new",
      "body": "Unlock is skipped on this error path; take the same `goto out` as [[src/lock.c:80]].\n\n<provenance trailer>" }
  ]
}
```

Always set the top-level `verdict` to `comment` — never `approve` or `request-changes`, which are the author's to set.

## Actions

- `comment` — a finding (opens a new thread).
- `summary` — the overall note; keep exactly one live summary. The last surviving summary is effective if several exist.
- `reply` — respond when the review calls for it; set `thread` to the bare forge id or the `remote:<id>` form from `show`.
- `drop` — withdraw a live action; set `id` to the one you're removing.

The implementation also accepts `resolve`, but this review workflow leaves thread state to the author and emits none.

## Where a comment anchors

Placement is inferred from which fields are present:

| Fields | Placement |
|---|---|
| `file` + `line` | a line comment |
| `file` only | a file-level comment |
| neither | an MR-level conversation comment |

Use the repo-relative post-image `files[].path`; on a rename or copy, `wits` supplies `old_path` for an old-side anchor. `side` is `new` for an added or context line, `old` for a removed one (default `new`); the line must exist on that side. For a span, add `start_line` as the start and keep `line` as the end; `start_side` defaults to `side`, and a differing value makes a cross-side span. Leave `commit` unset for a new comment so `draft` stamps the current snapshot; preserve or set it only when intentionally retaining an older snapshot anchor.

## Reference code — `[[path:line]]`

A `[[path:line]]` token in any body becomes a forge permalink on submit, so prefer it over pasting a path or snippet when you want the author to jump somewhere. Grammar: `path` (repo-relative), optional `:line` or `:start-end`, optional `@ref` to pin a commit/branch/tag (default: the reviewed head). Examples: `[[src/y.c:20]]`, `[[src/y.c:20-30]]`, `[[src/y.c]]`, `[[src/y.c:20@main]]`.

## The provenance trailer

Every `comment`, `reply`, and `summary` body ends with exactly one trailer line:

```markdown
**Generated-by:** <tool> (<model>) <!-- this comment is submitted by wits-review -->
```

Replace every angle-bracket token in the batch template before ingest, including `<provenance trailer>` with the exact line above. Fill `<tool>` and `<model>` with the identifiers exposed by the current session, using the most specific identifiers actually known without inventing a model variant. `wits` performs no provenance-placeholder substitution, so no placeholder may remain in the batch. The HTML comment renders invisibly on both forges.

## Revise or withdraw

The draft is append-only, keyed by `id` (`wits:<uuid>`, assigned when omitted). Append a revised action with the same id to replace it in the effective view; append `drop` with that id to withdraw it. Give genuinely new actions no id and let `draft` assign one. `wits review draft <mr> --dedup` may compact the stored stream when that is intentional.
