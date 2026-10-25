//! `wits-core` — the shared floor under the `wits` CLI and its plugins.
//!
//! Everything a command needs that isn't the command itself and isn't project
//! policy lives here: the thin OS/git/process floor, the Jinja language, and the
//! self-contained git, forge, and worktree subsystems. The `wits` binary and
//! the project layer both compose these; nothing here names a project, a build
//! system, or a backend.
//!
//! The modules are flat on purpose. There is still a rough gradient — `config`,
//! `editor`, `git`, `jinja`, `log`, `pager`, `path`, `process`, `time` are the
//! thin floor; `forge`, `remote`, `trunk`, `worktree` are subsystems with real
//! domain logic — but they sit side by side so a consumer names
//! `wits_core::process` or `wits_core::forge` directly, without a grouping layer
//! in between.
//!
//! The gradient is a real line, not a label. `jinja` is on the floor because it
//! defines a language and decides nothing; resolving a *config* against it —
//! where an entry may itself be a template naming another entry — is policy, so
//! it lives with the config it serves, in the project layer's context.
//!
//! Two pairs that were once separate modules are now unified where they belong:
//! `config` folds in the single-setting `Resolver` (both answer "where does this
//! come from?"), and the git-remote parsing that feeds forge detection lives in
//! `forge::remote` (re-exported from `forge`), beside the forge it serves.
//!
//! Two modules say "remote" and mean different halves of the word. [`remote`]
//! owns what a remote *is to us* — the `origin`/`upstream` role vocabulary and
//! which local remote holds each — and depends only on `git`, so `forge` can take
//! a resolved mapping without knowing the project registry exists. `forge::remote`
//! owns what a remote *URL says* — host, owner, repo, and therefore which forge to
//! call. Resolving roles from a *user's declaration* is neither, and lives with
//! the declaration, in the project layer.

pub mod config;
pub mod editor;
pub mod forge;
pub mod git;
pub mod jinja;
pub mod log;
pub mod pager;
pub mod path;
pub mod process;
pub mod remote;
pub mod system;
pub mod time;
pub mod trunk;
pub mod worktree;
