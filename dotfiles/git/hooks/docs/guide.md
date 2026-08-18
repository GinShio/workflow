# Git hooks — Guide

A shared set of git hooks that run in every repository, do the routine chores —
formatting, linting, catching secrets and stray conflict markers, keeping LFS
and per-branch bookkeeping in step — and otherwise stay out of your way. This is
how to turn it on, what each hook is for and how to tune it, and how to switch
any of it off. For *why* it is built this way, see the [design notes](design.md).

## Turning it on

Point git at this directory, once per machine:

```sh
git config --global core.hooksPath /path/to/scripts/git/hooks
```

A couple of orientation points before the details. Most of this is automatic and
silent; you mainly notice it when a `pre-commit` or `pre-push` check turns you
away, which is the point. Each configurable behaviour is described with its keys
below; how those keys are read — git config versus environment — is spelled out
once in [How settings are read](#how-settings-are-read).

Two external tools are assumed to be installed and ride along on several hooks
with no setup from you: **Git LFS** keeps large-file objects synced (on checkout,
merge, commit, and push), and **git-branchless** records history events for its
smartlog/undo (on nearly every hook). Neither has anything to configure, so the
sections below only cover the framework's own scripts.

## How settings are read

Every knob in this guide is a git config key under the `wits.hooks.`
namespace, and every one has an environment-variable twin. They cover different
needs:

- **git config** is the standing choice — `git config --local <key> <value>` for
  the current repo, `--global` for all of them. Reach for it when you want a
  lasting preference.
- **The environment variable** is a one-off for the command in front of you, and
  it overrides config when both are set — the "not this time" escape hatch.

The environment name is a pure mechanical transform of the config key: upper-case
the whole thing and turn every `-` and `.` into `_`. There is no prefix juggling
and no special case — the global kill switch `wits.hooks.disable` is simply
`WITS_HOOKS_DISABLE`.

```
wits.hooks.disable                            →  WITS_HOOKS_DISABLE
wits.hooks.pre-commit.disable                 →  WITS_HOOKS_PRE_COMMIT_DISABLE
wits.hooks.pre-commit.format-clang-disable    →  WITS_HOOKS_PRE_COMMIT_FORMAT_CLANG_DISABLE
```

Boolean keys accept the usual git spellings — `true`/`false`, `1`/`0`,
`yes`/`no`, `on`/`off`.

Every boolean switch reads its polarity from its suffix, so the default is always
"key unset" and you only ever set one to move off that default:

- a **`-disable`** key guards a behaviour that runs **by default** — set it to
  turn that behaviour off;
- an **`-enable`** key guards a behaviour that is **off by default** — set it to
  turn it on.

Throughout the sections below keys are written relative to their hook: a setting
shown as `format-clang-disable` under `pre-commit` is the full key
`wits.hooks.pre-commit.format-clang-disable`.

## `pre-commit`

Fires after you stage changes and before the commit is recorded. It is the
busiest hook here and the only one that can reject a commit — a failure stops the
commit with a note on what to fix, so problems surface now instead of in review.
To bypass the whole hook for one commit, `git commit --no-verify`.

### Formatter

Keeps the tree consistently formatted without you having to think about it, so
what you commit is already clean. Each language is handled independently — C/C++
through `clang-format`, Rust through `rustfmt`, Zig through `zig fmt`, Python
through `ruff` (falling back to `black` + `isort`) — and a generic pass ensures a
final newline on every other text file, trimming trailing whitespace where that
is safe. A language is handled only when its formatter is on your `PATH`.

The generic pass withholds the trailing-whitespace trim where it would corrupt
meaning — Markdown (trailing spaces are a hard line break) and CSV/TSV (a
trailing tab/space is a delimiter) keep their whitespace, and `patch`/`diff` are
left entirely untouched. Trailing whitespace is insignificant in most other
markup (LaTeX, HTML, rST), so those are trimmed; the exception is verbatim-style
blocks, so spare such a file by extension if you keep literal trailing spaces.

It formats the **staged content**, not the working tree: it rewrites the version
in the index and, when your working copy has no unstaged edits, updates that too.
A partially-staged file therefore keeps its unstaged changes intact — the commit
gets the formatted version, your in-progress edits are left alone.

Because each language is its own script, it is turned off through the ordinary
[per-script switch](#turning-pieces-off) — set the key, no special casing:

- `format-clang-disable`, `format-rust-disable`, `format-zig-disable`,
  `format-python-disable` — one per language, each on by default.
- `format-generic-disable` — the generic trim/final-newline pass, on by default.
- `format-generic-notrim` — a space-separated list of extra file extensions
  (e.g. `.tex .snap`) whose trailing whitespace to preserve, on top of the
  built-in Markdown/CSV/TSV set.
- `format-clang-style` — a named style (`llvm`, `google`, …) for C/C++ when the
  repo has no `.clang-format` of its own.

### Linter

Catches mistakes cheaply, before they reach a reviewer. Like the formatter, each
language is handled independently, running the fast, file-oriented static
analyzer over what you're committing — `ruff` (or `flake8`) for Python,
`zig ast-check` for Zig — and stops the commit on a finding. It works on the
**staged content** (a partially staged file is linted exactly as it will be
committed, not with your unstaged edits) and only runs where the tool exists.

Only genuinely *static* (no-build) linters live here. Languages whose analysis
requires a compile have no entry: **Rust** has no non-compiling linter (`clippy`
builds the crate, so it is left to CI rather than the commit path), and **C/C++**
is not linted for now (accurate analysis needs a compilation database and still
carries false positives). Both are still *formatted*, just not linted.

- `lint-python-disable`, `lint-zig-disable` — one per language, each on by
  default; set to turn that language's linter off.

### Sanity checks

A safety net for the mistakes that are easy to make and tedious to undo. It
refuses a commit that still carries an unresolved merge-conflict marker, one that
adds a symlink pointing nowhere, or one that would drag in an oversized file —
usually a fat-fingered `git add` of a build artifact or a dataset.

- `sanity-checks-max-file-size` — the size ceiling in bytes (default 25 MiB).

### Marker guard

Lets you plant a tripwire in your own code. Stage a line containing
`DO_NOT_SUBMIT`, `NOCOMMIT`, or `FIXME_BLOCKER` and the commit is refused until
you remove it — the reliable way to make sure a debug hack or a note-to-self
never ships. Nothing to configure.

### Secret scan

A last line against committing credentials. It scans the staged diff with
`gitleaks` (preferred) or `git-secrets` and blocks on a match. There is no switch
to flip: it is active whenever one of those scanners is on your `PATH`, so
installing the tool is how you turn it on. A genuine false positive can be waved
through with `git commit --no-verify`.

### Encoding check

Keeps staged text honest: LF newlines only, and valid UTF-8. A staged text file
carrying a CR/CRLF line ending or an invalid UTF-8 byte is rejected. Binary blobs
are skipped, and so is the UTF-8 half when `iconv` isn't available. Nothing to
configure — for automatic newline normalization on top of this, let git handle it
with a `.gitattributes` `text=auto eol=lf`.

### Protected-branch prompt

A guard against committing straight onto a shared branch by accident. Enabled, it
asks you to confirm before committing while a protected branch is checked out.
Off by default, since whether a direct commit is a mistake depends entirely on
how you work.

- `warn-protected-enable` — off by default; set true to get the prompt.

Which branches count as protected is shared by this prompt and the `pre-push`
one, and is configurable framework-wide:

- `wits.hooks.protected-branch` — an extended regular expression (matched with
  `grep -E`) a branch name must match to be treated as protected. Defaults to
  `^(main|master|dev|release-.*|patch-.*)$`. As with every key it has an
  environment twin, `WITS_HOOKS_PROTECTED_BRANCH`.

## `prepare-commit-msg`

Runs as git assembles the initial commit message, before your editor opens, which
is the moment to seed it with something.

### Issue-ID prefix

Saves retyping a ticket number into every commit. Enabled, it reads an issue ID
out of the current branch name and adds it to the message — so on
`feature/PROJ-123-login` your subject opens with `[PROJ-123] ` already there. It
applies to almost every message-producing command (a plain commit, `-m`, a `-t`
template, a merge, a cherry-pick, a revert, an amend, …) but stays out of the way
where it shouldn't act: it never rewrites messages during a `rebase` (replayed
commits already carry their ID), never touches an autosquash marker
(`fixup!`/`squash!`/`amend!`, so `git rebase --autosquash` still matches), never
runs on a detached HEAD (no branch to read from), and never adds itself twice.
Off by default so it never intrudes on repos that don't work this way.

By default the ID is **prepended** to the subject as `[PROJ-123] `. It can
instead be **appended** as a git trailer at the end of the message
(`Refs: PROJ-123`) — the right shape for tools that read trailers.

- `issue-tracker-enable` — off by default; set true to turn it on.
- `issue-tracker-regex` — what to pull from the branch name (default
  `[A-Z]+-[0-9]+`).
- `issue-tracker-position` — `prepend` (default) puts the ID at the start of the
  subject; `append` adds it as a git trailer at the end of the message body
  (placed correctly relative to git's comment block; idempotent on a re-run, but
  a *different* ID is accumulated rather than duplicated).
- `issue-tracker-format` — how to wrap the ID; the first `%s` is replaced by the
  ID and everything else is taken literally. The default follows the position:
  `[%s] ` for `prepend`, `Refs: %s` for `append` (which should stay
  trailer-shaped, `Token: %s`).
- `issue-tracker-default` — a fallback ID to use when the branch name carries
  none. In `append` mode this placeholder is added only when the message has no
  issue trailer yet (it never stacks onto a real, branch-derived reference),
  whereas a branch-derived ID accumulates a distinct value as described above.

### Provenance trailer

Records where a *derived* commit came from as a structured git trailer, so the
link is machine-readable instead of buried in prose (or, for a cherry-pick
without `-x`, absent entirely):

- a merge → `Merges: <sha>` (one per parent, so an octopus merge lists them all);
- a cherry-pick → `Cherry-picked-from: <sha>`;
- a revert → `Reverts: <sha>`, *replacing* git's default `This reverts commit
  <sha>.` line.

The operation is detected from git's in-progress marker files, so it works for a
cherry-pick or revert done without an editor. It stays out of a `rebase`, and the
trailer is idempotent on a re-edit. On by default — it only touches
merge/cherry-pick/revert and is otherwise additive.

## `pre-push`

Runs before git hands refs to a remote, and can abort the push.

### Protected-branch prompt

The push-side counterpart to the commit prompt: enabled, it asks for confirmation
before pushing to a protected branch on the remote. Off by default. The set of
protected branches is the shared `wits.hooks.protected-branch` regex documented
under [`pre-commit`](#protected-branch-prompt).

- `warn-protected-enable` — off by default; set true to get the prompt.

## `post-checkout`

Runs after `git checkout`/`switch` and after `clone`. Everything here is advisory
— it never blocks the operation, it just keeps your working environment in step
with the branch you moved to.

### Workspace restore

Keeps your editor pointed at the right build as you jump between branches. For an
out-of-tree build it repoints the working tree's `compile_commands.json` at the
active branch's build directory, so language servers and clang-tooling index the
branch you're actually on. It only manages that path when it is a symlink or
absent, and never clobbers a real file you keep in-tree. Nothing to configure.

### Dependency-change warning

A nudge so you don't run against stale dependencies. When a checkout changes a
lockfile — `package-lock.json`, `Cargo.lock`, `go.sum`, `poetry.lock`, and the
rest — it reminds you to reinstall. It only ever warns; it won't run your package
manager for you. (The same script also runs on `post-merge`.)

### Maintenance enrolment

Opts each repo into git's own upkeep. The first time you land in a repo it
registers it with `git maintenance start`, so git's background tasks
(commit-graph, gc, and so on) keep the repo fast without you scheduling anything.
Nothing to configure.

### Branchless bootstrap

Sets up `git-branchless` for a repo the first time you check out a branch there —
typically right after cloning — when branchless is installed and not already
initialized, so a fresh clone is ready without a manual `git branchless init`.

## `reference-transaction`

Fires whenever refs change. The scripts here react to one case in particular —
branch deletion — and act only once the change is actually committed, so an
aborted rebase or a rolled-back transaction never triggers them.

### Machete cleanup

Keeps your `git-machete`/stack layout honest as branches come and go. Delete a
branch and it is removed from `.git/machete` with its children spliced up to its
parent, so the tree stays valid instead of collecting dangling entries you'd have
to prune by hand. Runs wherever a machete file exists; nothing to configure.

### Build-directory cleanup

Reclaims disk when you delete a branch. Opt in, and deleting a branch also removes
its build directory (resolved through the project's `builder.py`). Off by default
because it deletes files — enable it per repo where you want the housekeeping.

- `cleanup-build-dir-enable` — off by default; set true to remove build
  directories on branch deletion.

## The recorder hooks

`post-commit`, `post-merge`, `post-applypatch`, `post-rewrite`, and `pre-auto-gc`
mostly exist to feed the two pass-through integrations — `git-branchless` on all
of them, Git LFS on `post-commit` and `post-merge`. `post-merge` additionally
reuses the dependency-change warning above. There is nothing framework-specific
to configure on any of them.

## Turning pieces off

Beyond the per-behaviour switches above, you can silence things wholesale — when
a hook is wrong for a repo, or simply in your way this once. Three scopes:

- **Everything** — `wits.hooks.disable`
- **One hook** — `wits.hooks.<hook>.disable`, e.g. `wits.hooks.pre-commit.disable`
- **One script** — `wits.hooks.<hook>.<script>-disable`, naming the script
  without its numeric prefix, e.g. `wits.hooks.pre-commit.format-python-disable`

As everywhere, config is the standing choice and the environment variable is the
one-shot that wins for a single command (see [How settings are
read](#how-settings-are-read)):

```sh
git config wits.hooks.pre-commit.disable true      # this repo, from now on
WITS_HOOKS_PRE_COMMIT_DISABLE=1 git commit …        # only this commit
```

## Extending the pipeline

When a hook fires, its scripts run in a fixed order — each stage additive, and
fail-fast (the first non-zero exit stops the hook):

1. the built-in `<hook>.d/` scripts;
2. any **overlay layers** (`secret-*`, below);
3. the **external hooks** you point the framework at;
4. the repository's own `.git/hooks/<hook>`, if it still has one.

A candidate runs only if it is **executable** and its first line is a `#!`
shebang — anything else (a data file, or a still-encrypted blob) is skipped
rather than executed. Every stage obeys the same
[disable hierarchy](#turning-pieces-off).

### Adding a built-in check (e.g. another language)

Every behaviour is one executable, so adding one is dropping a file into the
right `<hook>.d/`. The formatter and linter are split one-language-per-file
precisely for this: to add a language, copy the closest sibling, point it at the
tool, and list the extensions. The `staged_lang_files` helper does the
staged/text/binary/encrypted filtering, so the script stays a few lines:

```sh
#!/bin/sh
. "$HOOKS_DIR/core/lib.sh"
files=$(staged_lang_files .go)          # staged .go files, text only
[ -n "$files" ] || exit 0
command -v golangci-lint >/dev/null 2>&1 || exit 0
# ... run the tool on the staged content, exit non-zero to block the commit
```

Peers of one concern share a numeric prefix (every formatter is `50-`, every
linter `60-`); the prefix only sets run order, so the shared number just says
"these are the same stage." The clean name (prefix stripped) is the
[toggle key](#turning-pieces-off) automatically — a `60-lint-go` answers to
`wits.hooks.pre-commit.lint-go-disable` with no extra wiring. Nothing to
register.

### External hooks (Husky, `.githooks`, custom scripts)

If a repo already carries hooks, they can run in the same pass instead of being
displaced. There are two ways to point at them; both accept colon-separated
entries, absolute or relative to the repo root, and both run *after* the built-in
and overlay scripts so a project's own checks get the last word.

**Directories to scan** — `wits.hooks.external-dirs`. In each directory, for a
given hook, the framework runs whichever of these it finds:

- a single executable file named exactly `<hook>` — the Husky convention
  (`.husky/pre-commit`);
- a `<hook>.d/` directory, whose executable scripts run in filename order, exactly
  like the built-in ones (`.githooks/pre-commit.d/`).

**Explicit scripts** — `wits.hooks.<hook>.external-scripts`, for a project whose
scripts don't follow the `dir/<hook>` convention. They run in the order listed.

```sh
git config wits.hooks.external-dirs ".husky:.githooks"
git config wits.hooks.pre-commit.external-scripts "scripts/lint.sh:tools/check-fmt"
```

A scanned directory's single-file hook is toggled under the pseudo-name
`external` (`WITS_HOOKS_PRE_COMMIT_EXTERNAL_DISABLE=1`); explicit scripts and a
directory's `.d/` scripts are toggled under their own filename.

### Overlay layers (`secret-*`)

Alongside the hooks directory you can drop overlay directories named `secret-*`,
each mirroring the main layout (`secret-<name>/<hook>.d/`). Their scripts run
right after the built-in ones and before any external hooks — a home for private
or machine-specific hooks you don't want in the shared tree.

This is also where a **transparently-encrypted** hook belongs. Keep a script under
`secret-<name>/<hook>.d/` and encrypt it with transcrypt: it sits in the repo as
ciphertext, and because a script only runs when it starts with a `#!` shebang, the
encrypted (shebang-less) blob is quietly skipped until it's decrypted on checkout
— after which it runs like any other. So a private hook can travel in the repo
without exposing its contents, and without erroring on a machine that can't
decrypt it.

## Seeing what a hook is doing

When something surprises you, turn up the logging for the next command — the
levels run from silent to a full shell trace:

```sh
WITS_HOOKS_LOG_LEVEL=3 git commit …   # 0 silent · 1 errors · 2 warnings (default) · 3 info · 4 trace
```
