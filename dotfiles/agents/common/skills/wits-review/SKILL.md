---
name: wits-review
description: Review or re-review a numbered MR/PR locally with `wits review` — refresh and materialize its snapshot in the dedicated review worktree, inspect the full tree and stored history, then draft or revise `local.json` without submitting. Use for numbered merge/pull-request reviews and local wits review drafts; judgment and wording come from `review-protocol`.
---

# Wits Review

Review a numbered MR/PR through `wits review`. Work from its dedicated, reusable review **worktree**, where the full tree is available, and persist the result only as a local **draft** the author later submits.

Apply `review-protocol` for what to flag, how to weigh and word it, and any report to the user. Use this skill only for the `wits review` mechanics.

## Read-only, draft-only

- Treat the checked-out source as read-only, and keep the main worktree's files and `HEAD` unchanged. Use `wits review checkout` for every review-worktree `HEAD` change; never use `checkout --in-place` or raw `git checkout`/`switch`/`reset`/`restore` in either worktree.
- Let `fetch` be the only forge access, and use it only to read. Keep authored output local: never `submit`, `push`, or write through `gh`/`glab`.
- Draft commentary only: set `verdict` to `comment`, emit no `resolve` action, and use `reply` only when the review calls for a response to an existing thread. The tool supports other verdicts and resolution; this workflow leaves those decisions to the author.

## 1. Refresh and materialize the review

Start from the forge's current snapshot and the complete local review state:

- Run `wits review fetch <mr>` at the start so the snapshot, discussion, and stack state are current.
- Run `wits review checkout <mr>`. It creates or repoints the one reusable review worktree; use the path it reports, `cd` there, and run the rest of the review from it. `--next`/`--prev` repoint that same worktree within the stack.
- Read `wits review show <mr> --json` for `snapshot.fork_sha`, `snapshot.head_sha`, snapshot history, commits, files, threads, and neighbors. Read `wits review draft <mr> --json` for the exact pending action stream before changing it.

Done when the refreshed `head_sha` is checked out in the review worktree and you hold its `fork_sha`, history, files, threads, neighbors, and existing draft.

## 2. Read the change in full context

Read past the diff: read entire files, follow symbols, and run project tooling from the review worktree.

- **Current change:** read `wits review diff <mr> --patch`. This is the patch the final review assesses and the source of comment line/side coordinates.
- **Re-review:** identify the newest earlier `snapshots[].head_sha` also attached to the existing local or remote comments. Read `wits review diff <mr> --against <old-head> --patch` for the change since that review point. If no comment identifies one, assess the current patch instead of guessing. When the comparison reports identical-content ambiguity, inspect the same comparison without `--patch`, then with `--patch=3way`.
- **Around the change:** read the changed files whole and use `rg` to follow each affected symbol to its definition and other uses, so you see the change's reach.
- **Before/after:** the worktree holds the post-image. Read the pre-image with `git show <fork_sha>:<pre-image-path>`, where `<pre-image-path>` is `files[].old_path` for a rename or copy and `files[].path` otherwise.
- **Intent:** read `git log <fork_sha>..<head_sha>` and the relevant `git show <sha>` output.
- **The stack:** walk MRs below this one with `wits review checkout --prev` far enough to understand the state this MR assumes, then restore the target with `wits review checkout <mr>`.
- **Behavior:** use the project's documented build/test/run recipe when execution can establish behavior more reliably than inspection.

Done when every changed file has been read in full, every affected symbol and lower-stack assumption is understood, the commit intent is known, and any relevant re-review delta and executable checks have been assessed.

## 3. Draft the review into `local.json`

Read [`draft.md`](draft.md) completely before the first write. Translate the `review-protocol` result into one effective `summary` plus one `comment` per finding; LGTM is the `summary` with no finding comments. Keep `verdict: comment` for every outcome — it is the transport policy, while the review assessment lives in the bodies.

Revise an existing draft by reusing action ids and withdraw obsolete actions with `drop`, then pipe the batch to `wits review draft <mr> -`. Re-read both `wits review draft <mr> --json` and `wits review show <mr> --json` after ingest to verify the stored actions and their effective folded view.

Done when the effective draft contains exactly one summary, one correctly anchored comment per finding, no resolution action, no duplicate live action, the required provenance trailer on every body, and `verdict: comment`; nothing has been submitted.
