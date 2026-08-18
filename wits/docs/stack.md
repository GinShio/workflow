# `wits stack`

Turn a chain of local branches into a set of merge requests that reviewers can
actually navigate — and keep them in sync as you reshape the stack. You do the
local work however you like (`git rebase`, `git-branchless`, plain commits);
`wits stack` handles the remote half: pushing the branches, opening an MR for each
against the right base, and writing a navigation block into every MR so a
reviewer can walk the whole stack.

> Terminology: GitHub calls it a *pull request*, GitLab a *merge request*. This
> tool calls it an **MR** everywhere; on a GitHub repo the output just says "PR".

## The mental model

Three facts make up the state of a stack on the remote, and there is one verb
for each. They are independent on purpose — run any one of them on its own, and
re-run it freely; each only reconciles its own slice of the world.

| Verb | Owns | Touches |
|---|---|---|
| `wits stack sync` | branch **content** on the remote | git only (push) |
| `wits stack submit` | MR **existence** and **base** | the forge API |
| `wits stack anno` | MR **descriptions** | the forge API |

Plus a few helpers: `wits stack slice` cuts commits into the stack in the first
place, `wits stack decorate` adds labels/reviewers/assignees to an MR, and
`wits stack tree` edits the stack's structure.

The dependency tree itself lives in `.git/machete` (the same format
`git-machete` uses): one branch per line, indentation meaning "sits on top of".

```
main
    feature-api
        feature-ui
    feature-docs
```

Here `feature-api` and `feature-docs` both build on `main`; `feature-ui` builds
on `feature-api`. You don't have to hand-write this file — `slice` generates it —
but it is plain text and safe to edit.

> For the precise rules — how scope is chosen on a fork, how forks render, what
> happens when you add or remove a branch mid-stack — see the
> [behaviour reference](stack/behavior.md). This guide stays at the
> getting-things-done level.

## One-time setup

### A token for your forge

Opening and editing MRs needs an API token. Put it in git config (per host is the
most precise; a blanket key works too):

```sh
git config wits.forge.github.com.token  ghp_xxx
# or, less specific:
git config wits.forge.github.token       ghp_xxx
git config wits.forge.token              ghp_xxx
```

Or supply it through the environment, which always works and is handy on CI:

```sh
export GITHUB_TOKEN=ghp_xxx     # GITLAB_TOKEN / GITEA_TOKEN / FORGEJO_TOKEN / CODEBERG_TOKEN
```

`sync` needs no token (it only pushes); `submit` and `anno` do.

### Remotes: `origin` and `upstream`

`wits stack` reads two remotes by role:

- **`origin`** — where it pushes, and the source side of every MR. You need push
  rights here.
- **`upstream`** — the repository MRs merge *into*. Set this when you work on a
  fork; leave it unset when you push and merge in the same repo (then `origin`
  plays both parts).

```sh
git remote add origin   git@github.com:me/project.git
git remote add upstream git@github.com:acme/project.git   # only if you forked
```

The forge (GitHub/GitLab/Gitea/Forgejo/Codeberg) is detected from the
**upstream** URL — or `origin` when there's no upstream. A self-hosted instance
behind a custom domain can be named explicitly:

```sh
git config wits.forge.git.acme.com.service gitlab
git config wits.forge.git.acme.com.api-url https://git.acme.com/api/v4
```

## Building a stack with `slice`

`slice` cuts the commits sitting on top of your base branch into named branches.
It opens an interactive rebase todo seeded with your commits and a commented
branch suggestion under each:

```sh
wits stack slice              # slices <base>..HEAD
wits stack slice --base main
```

```
pick a1b2c3d Add the API layer
# update-ref refs/heads/me/add-the-api-layer

pick d4e5f6a Wire up the UI
# update-ref refs/heads/me/wire-up-the-ui
```

Uncomment the `update-ref` lines where you want a branch to start, save, and let
the rebase finish. The branches are created at the end of the rebase (safe even
for the branch you're on), and `.git/machete` is written to match. Branch-name
suggestions use `wits.stack.prefix` if set, otherwise a slug of your
`user.name`, otherwise `stack/`.

You don't *have* to use `slice` — any branches you create yourself and record in
`.git/machete` work identically. And a branch that isn't in the file at all is
treated as a one-branch stack (see [Single branches](#single-branches)).

### Growing or reshaping an existing stack

When you re-run `slice`, branches already in the stack come **pre-filled** in the
todo (active, under their real names), so re-slicing preserves them — you only
edit the lines you want to change.

```sh
# Append: you've added commits on top of a finished stack. Slice from the tip,
# so only the new commits are in play; uncomment a name for each.
wits stack slice --base feature-c

# Insert/rebuild: slice from the line's base. The existing branches are already
# active in order — just uncomment the new middle branch (and reorder commits as
# needed). The downstream branch is moved under it automatically, no leftovers.
wits stack slice
```

## The everyday loop

After reworking your commits:

```sh
wits stack sync       # push every branch in the stack to origin
wits stack submit     # open MRs that don't exist; fix bases that moved
wits stack anno       # refresh the navigation block in each MR description
```

Run them in that order the first time; afterwards run whichever matches what
changed. Reordered the stack but didn't touch code? `submit` alone fixes the MR
bases. Just amended a commit? `sync` alone re-pushes.

### Scope: which branches each verb touches

By default a verb acts on the stack around the branch you're standing on, with
one rule that's worth knowing:

- **On a linear branch** (zero or one child): it acts on *that line of work* —
  your ancestors, you, and the primary downstream chain. Sibling branches that
  fork off elsewhere are left alone.
- **On a fork-point** (two or more children): it acts on the *whole tree* you're
  the root of — every branch below you, plus your ancestors.

Pass `--all` to act on every stack recorded in `.git/machete`, regardless of
where you're standing.

```sh
wits stack sync --all
wits stack submit --all
```

You can also name a branch to anchor on, instead of checking it out — useful for
driving another stack from a worktree or a dirty tree:

```sh
wits stack submit feature-api     # submit the whole stack around feature-api
wits stack sync feature-api       # push that stack, without switching to it
```

The branch is a **scope anchor**, not the single target: the whole stack around
it is operated on, exactly as if you'd checked it out (an anchor mid-line still
pulls in its ancestors and downstream chain). That is the per-stack meaning —
different from `decorate`, whose branch names the one MR to touch. The anchor
must be a real branch (a local ref, or a name recorded in `.git/machete`), and it
can't be combined with `--all`.

The base branch (`main`/`master`/…) is never pushed and never gets an MR, but it
does appear in the navigation chains so reviewers see the full lineage.

### Drafts

A mid-stack MR — one whose base is *another* branch, not the base branch — is
opened as a **draft** by default, because it shouldn't be reviewed or merged
before the change it sits on. The MR at the bottom of the stack (targeting the
base branch) is opened ready. To open everything ready for review:

```sh
wits stack submit --no-draft
```

### MR title and body

A new MR's title and body come from one of the branch's commits — the newest by
default. Change which one:

```sh
wits stack submit --title-source first   # oldest commit instead
```

(Existing MRs are never re-titled; this only seeds creation.)

## Single branches

Not everything is a tall stack. A branch that isn't recorded in `.git/machete`
is treated as its own one-node stack sitting on the base branch — so `sync` and
`submit` work on an ordinary feature branch with zero setup:

```sh
git switch -c quick-fix
# ... commit ...
wits stack sync && wits stack submit
```

`anno` skips a lone branch: a single MR has no neighbours to navigate to.

## Labels, reviewers, and assignees

`wits stack decorate` adds labels, assignees, and reviewers to an MR. It is
**additive** — it only adds what you name and never removes anything — so it
never undoes a project's own label/reviewer bots, and re-running is safe.

Because these differ from one MR to the next, `decorate` works on **one MR at a
time** by default (the named branch, or the current one):

```sh
wits stack decorate feature-api --label api --reviewer alice --assignee @me
wits stack decorate              --label wip            # the current branch's MR
wits stack decorate --all        --label stacked        # the same label on every MR in the stack
```

`--label`, `--reviewer`, and `--assignee` are each repeatable; `@me` means you.

There is no config and no stored defaults on purpose. To make a project's
"always add these" behaviour, put the flags in a small per-repo script or your CI
step — since `decorate` is additive and idempotent, running it there repeatedly is
exactly equivalent to a default:

```sh
# a repo's dev script
wits stack sync && wits stack submit && wits stack anno
wits stack decorate feature-api --label api --reviewer alice
wits stack decorate feature-ui  --label ui  --reviewer bob
wits stack decorate --all       --label stacked
```

Attribute changes are best-effort: an unknown label or a reviewer the platform
won't accept is warned about and skipped, without failing the rest.

## Maintaining the stack structure

`slice` writes the stack; `wits stack tree` is for editing it afterwards. Removing
a branch never throws away what's stacked above it — its children splice up to
its parent, and the next `submit` retargets their base.

```sh
wits stack tree prune              # drop entries whose branch no longer exists
wits stack tree rm feature-b       # remove one branch (its children move up)
wits stack tree rm feature-b --delete   # ...and delete the git branch too
wits stack tree mv feature-c --onto main   # restack a branch (its substack moves with it)
```

`tree mv` updates the *shape* only; rebase the branch onto its new parent
yourself for the code to match. It also creates the entry if the branch wasn't in
the stack yet, so it doubles as "put this branch onto X".

### Automating cleanup

`tree prune` is the one to reach for in automation: it needs no branch names,
is idempotent, and only drops branches whose ref is actually gone. There is no
git hook for branch deletion (and `git maintenance` only runs its own built-in
tasks), so the clean integrations are either to run it at the end of a
branch-cleanup script:

```sh
git branch -d old-feature && wits stack tree prune
```

or on a timer (cron / systemd / launchd) the same way you'd schedule any
periodic chore. Since it's a no-op when nothing dangles, running it often is
harmless.

## Previewing with `--dry-run`

`-n`/`--dry-run` is global. It still reads from git and the forge to work out
what it *would* do, then prints the pushes, MR creations, base changes, and
description edits instead of performing them:

```sh
wits stack submit -n
wits stack sync -n -v        # -v also shows the underlying git commands
```

## Invocation forms

Like every `wits` tool, `stack` has direct forms via symlink — `wits-stack`,
`wits.stack`, or a bare `stack` (see the top-level [README](../README.md)).

## Configuration reference

All keys live under git config's `wits.*` namespace — forge identity and tokens
under the shared `wits.forge.*`, and `stack`'s own settings under `wits.stack.*`.

| Setting | Key | Notes |
|---|---|---|
| Token (per host) | `wits.forge.<host>.token` | Most specific; `<host>` is e.g. `github.com` |
| Token (per service) | `wits.forge.<service>.token` | `<service>` ∈ github, gitlab, gitea, forgejo, codeberg |
| Token (blanket) | `wits.forge.token` | Last config fallback |
| Token (env) | `GITHUB_TOKEN`, `GITLAB_TOKEN`, `GITEA_TOKEN`, `FORGEJO_TOKEN`, `CODEBERG_TOKEN` | Used when no config key matches. |
| Service override | `wits.forge.<host>.service` | Name a self-hosted host's type |
| API base override | `wits.forge.<host>.api-url` | For self-hosted / enterprise endpoints |
| Branch prefix | `wits.stack.prefix` | `slice` name suggestions (default: slug of `user.name`, else `stack/`) |

There is intentionally **no** base-branch config key: the base is resolved from
the merge target's remote HEAD, then `main`/`master`/`trunk`. (A future
`wits project` will supply it from project identity.)

Per-run choices — drafts (`--no-draft`), title source (`--title-source`), force
(`--force`), scope (`--all`) — are flags, not config, because they describe one
invocation rather than a standing preference.

## How it resolves things

The short version: the **base branch** comes from the merge target's remote HEAD
(`upstream`, else `origin`), then `main`/`master`/`trunk`; each **MR's base** is
its parent in `.git/machete` (or the base branch at a root); **cross-fork** MRs
work on all platforms (GitHub/Gitea via an `origin-owner:branch` head, GitLab via
its cross-project API). The full rules, including fork scope and dynamic edits,
are in the [behaviour reference](stack/behavior.md).

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `no API token for …` | Set `wits.forge.<host>.token` or the platform's `*_TOKEN` env var. |
| `could not detect the forge for host '…'` | Self-hosted behind a custom domain: set `wits.forge.<host>.service`. |
| `submit` fails to create an MR ("head not found" or similar) | The branch isn't on `origin` yet — run `wits stack sync` first. |
| A closed MR isn't reopened | Intended: a closed/merged MR at the current commit is left alone. Pass `--force` to recreate. |
| `on the base branch '…'` | You're standing on `main`. Check out a stack branch (or use `--all`). |
| `detached HEAD` | Check out a branch, or use `--all` to act on all recorded stacks. |
| `could not determine the base branch` | No remote HEAD and no `main`/`master`/`trunk`. Create the branch or set the remote's HEAD (`git remote set-head origin -a`). |
| `rebase did not complete` (from `slice`) | The interactive rebase was aborted or hit a conflict; finish or `git rebase --abort`, then retry. |
