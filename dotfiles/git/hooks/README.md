# Git hooks

One set of git hooks, shared across every repository through `core.hooksPath`,
that handles the routine chores — formatting, linting, secret scanning,
protected-branch guards, per-branch bookkeeping — and coexists cleanly with the
tools that also touch your hooks (`git-branchless`, Git LFS, Husky,
`git-machete`).

```sh
git config --global core.hooksPath /path/to/git/hooks
```

That is the entire install. Each hook is a small stub that hands off to
`core/runner`, which runs the enabled behaviours in `<hook>.d/` in order; the
shared state and helpers live in `core/lib.sh`.

| Hook | Runs |
|---|---|
| `pre-commit` | protected-branch prompt (opt-in), sanity checks, marker guard, encoding, secret scan, formatters, linters |
| `prepare-commit-msg` | issue-ID prefix from the branch name (opt-in) |
| `pre-push` | protected-branch prompt (opt-in), Git LFS |
| `post-checkout` | branchless init/record, LFS, encrypted-file modes, workspace restore, lockfile warning, maintenance |
| `post-merge` | branchless record, LFS, encrypted-file modes, lockfile warning |
| `post-commit` / `post-applypatch` / `post-rewrite` / `pre-auto-gc` | branchless recorders (+ LFS on commit, encrypted modes and lockfile warning on rewrite) |
| `reference-transaction` | branchless record, machete cleanup, build-dir cleanup (opt-in) |

## Docs

The documentation is a reStructuredText book under `docs/`, written to be read
as plain text — open the files and read them in order:

- **[index](docs/index.rst)** — overview, install, and the hook↔behaviour map.
- **[guide](docs/guide.rst)** — how to drive it: activation, a walk through
  what each hook does and the settings that change it, and how to turn pieces
  off or bring your own hooks.
- **[config](docs/config.rst)** — the complete reference: every config key, its
  default, and its environment-variable twin.
- **[design](docs/design.rst)** — why it is shaped this way: the stub/runner
  split, the `.d` execution contract, the layering, and the portability rules.
