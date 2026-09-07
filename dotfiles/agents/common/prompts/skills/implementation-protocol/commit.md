# Commit messages

The diff shows what changed. The message exists for what the diff cannot show: the problem, the motivation, the reasoning a reviewer needs now and a contributor needs years from now — often the author themself (FreeBSD: imagine revisiting the change a year or two in the future, and write the message that provides that context). So write **what and why**, not how — a message that narrates the diff ("update X", "clean up Y") repeats what is already visible and carries nothing to a reader who cannot see it. A body that keeps growing is the signal to split the change, not to compress it.

## Subject

`<area>: <imperative summary>`

- **Area prefix** — read `git log` on the files being changed first, and reuse the repo's existing prefix form (FreeBSD: "try to use the same prefix used in previous commits to the same files").
- **Imperative mood** — an order to the tree that finishes "when applied, this change will ...". Kernel's example: "make xyzzy do frotz" — never "[This patch] makes xyzzy do frotz" or "[I] changed xyzzy to do frotz".
- **Size and shape** — aim for ~50 characters, hard bound ~67 so `git log --oneline` stays single-line with the prefix included; first letter capitalized after the prefix unless the repo's history says otherwise (the two standards disagree here — kernel examples lowercase, FreeBSD capitalizes); no trailing punctuation — a headline, not a sentence.
- **What and why** — the summary is the search key every future `git log --grep` runs against; kernel: it "must describe both what the patch changes, as well as why the patch might be necessary".

## Body

Problem first, then solution, then evidence. Every element earns its place by being invisible in the diff:

- **Problem** — what is wrong and why it matters: the user-visible impact (crash, leak, wrong output, regression) or the requirement that forces the change. State it even when the change came out of review — a commit with no stated problem claims no necessity.
- **Solution** — what is done about it and why this way over the alternatives that were close; the invariant or constraint it preserves; limitations named, not hidden. When a sentence here is really explaining tricky code, consider moving it into a comment instead — one context, one home, per `COMMENTS.md`.
- **Evidence** — numbers for every performance claim, with how they were measured, and the non-obvious costs; symptoms worth searching for (log excerpt, backtrace) distilled, not pasted; the anchor the change answers to — at least 12 sha characters plus the subject line, `Fixes: <sha> ("<subject>")` — and PR, tracker, or discussion links where they exist, while keeping the message self-contained: summarize the point inline, because links rot.

Wrap the body at 72 columns; separate the footer tags from the body with a blank line. Footer tags follow the repo's own conventions — read neighbouring commits (kernel: `Fixes:` / `Closes:` / `Link:`; FreeBSD: `PR:` / `Reviewed by:` / `Differential Revision:` last).

## Attribution trailer

Every commit you are asked to make carries an AI attribution trailer. Trailer keys follow open-source community convention:

- `Assisted-by` — you contributed to decisions or generated parts of the code, but the user directed the design and significant portions.
- `Generated-by` — you generated almost all of the code in the commit.

The value format is `<TOOL> (<MODEL>)`. `<TOOL>` is the AI coding tool in use (e.g. `Claude Code`); `<MODEL>` is the active model. Omit the model parenthetical when it can't be determined.

When in doubt, `Assisted-by` — it covers the common case where the commit is an approved design turned into code by you. The kernel's submitting-patches doc now requires `Assisted-by:` "if you used any sort of advanced coding tool in the creation of your patch", so these keys keep that alignment while recording who directed the work.

---

Distilled and pinned: [kernel submitting-patches](https://docs.kernel.org/process/submitting-patches.html) ("Describe your changes", the canonical subject line and body) and [FreeBSD committers guide](https://docs.freebsd.org/en/articles/committers-guide/#commit-log-message) (§Commit Log Messages). Where they conflict, the repo's own history decides.
