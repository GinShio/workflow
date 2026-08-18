---
name: wits-review
description: Review a numbered MR/PR locally with `wits review` — check it out into an isolated sibling worktree for full on-disk context, then draft findings into a `local.json` you never submit. Use when reviewing a numbered merge/pull request locally, drafting review comments, or driving the wits review store; judgment and phrasing come from `review-protocol`.
---

# Wits Review

Review a numbered MR/PR through `wits review`: you check the MR out into an isolated sibling **worktree** and do the whole review from there, with the full tree on disk for context, and your one output is a local **draft** the author later submits themselves. Your own working tree is never touched.

Judgment and phrasing are not here — what to flag, how to weigh it, how to word it, and any report you give the user come from the `review-protocol` skill. This skill is how you drive the tool.

## Read-only, draft-only

- Work inside the review worktree, and leave your own tree untouched — never move its `HEAD`: no `wits review checkout --in-place`, and no `git checkout`/`switch`/`reset`/`restore` in your own worktree.
- `fetch` is the only forge call, and it only reads; everything you find stays in the local draft until the author submits. Don't `submit`, `push`, or write through `gh`/`glab`.
- The draft only comments: always set `verdict` to `comment`, and leave threads as they are. Approving, requesting changes, and resolving are the author's at submit time.

## 1. Initialize the review in its worktree

Get the MR onto disk on its current snapshot, then work from there:

- `wits review fetch <mr>` acquires or refreshes the MR. Re-fetch when the snapshot is stale — an old `updated_at`, or the author just pushed — so you review the current head, not a superseded one.
- `wits review checkout <mr>` materializes that snapshot in an isolated sibling worktree (`../<repo>.review`), which leaves your own tree alone. `cd` there and run the rest of the review from inside it; `--next`/`--prev` move to another MR in the stack.
- `wits review show <mr> --json` gives the metadata (`threads`, `neighbors`, `updated_at`); `wits review diff <mr> --json` gives the `base_sha`/`head_sha`, files, and commits.

Done when the current snapshot is checked out in its worktree, you are working inside it, and you hold its base/head SHAs, files, threads, and neighbors.

## 2. Read the change in full context

You have the whole tree on disk at head, so read past the diff — read entire files, follow symbols, and run tooling directly in the worktree:

- **The patch:** `wits review diff <mr> --patch` shows the change line by line. A `new`-side line is added or context; an `old`-side line was removed.
- **Around the change:** read the changed files whole and grep the worktree to follow a symbol to its definition and other uses, so you see the change's reach.
- **Before/after:** the worktree is the new file; `git show <base_sha>:<path>` gives the pre-image when you need to compare.
- **Intent:** `git log <base_sha>..<head_sha>` and `git show <sha>` for the commit messages.
- **The stack:** when `neighbors` lists MRs beneath this one, `checkout --prev` and read them the same way — this change assumes the state they establish, so judge it against that base, not the trunk alone.
- **Build or run it:** the worktree is a real checkout, so compile, run, or fuzz the code here whenever behavior is easier to confirm than to reason about.

Done when every changed file has been read in full with its surrounding context, and any stack beneath it is understood.

## 3. Draft findings into `local.json`

Record findings by piping a JSON batch to `wits review draft <mr> -`. The action shape — where a comment anchors, how to reference other code with `[[path:line]]`, the required trailer, and how to revise or withdraw — lives in [`draft.md`](draft.md); read it before your first write.

Done when every finding is an action in the batch, each body ends with the provenance trailer, and `verdict` is set to `comment`.
