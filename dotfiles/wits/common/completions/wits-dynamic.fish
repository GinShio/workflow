# wits — dynamic value completion: the parts the generator cannot know.
#
# The static skeleton (subcommands, flags, enum values) is generated from the
# clap tree into ~/.config/fish/completions/wits.fish by `wits __completions
# fish`; the regenerate recipe is pinned beside the installs in
# dotfiles/wits/manifest.toml. This file holds only values queried from the
# world: it changes when a data source moves, not when a flag does. Sourcing
# at startup only registers completions and defines functions; every query
# runs at TAB time.

# Projects: `wits project` prints one `org/name focus=… build=…` row per
# project. Both the bare name and org/name are accepted as [NAME|PATH]
# (wits/docs/commands/project.rst), so the candidate is the bare name with
# org/name as the description.
function __wits_projects
    wits project 2>/dev/null | while read -l row
        set -l orgname (string split -f 1 ' ' -- $row)
        string match -q '*/*' -- $orgname; or continue
        printf '%s\t%s\n' (string split -m 1 -f 2 / -- $orgname) $orgname
    end
end
complete -c wits -f -n '__fish_seen_subcommand_from build update' -a '(__wits_projects)'
complete -c wits -f -n '__fish_seen_subcommand_from project' -a '(__wits_projects)'

# Branches: local branches of the current repo. Self-contained — fish 4.x
# removed the old __fish_git_branches helper, and depending on which of its
# internal replacements exists would tie this file to one fish version. wits
# ships no branch-listing query; for the stack and worktree verbs the current
# repo is the one in play, and -b/--branch is normally typed inside the focus
# repo — from elsewhere the completion is simply empty rather than wrong.
function __wits_git_branches
    git for-each-ref refs/heads --format='%(refname:short)' 2>/dev/null
end
complete -c wits -f -n '__fish_seen_subcommand_from stack; and __fish_seen_subcommand_from sync submit anno decorate' -a '(__wits_git_branches)'
complete -c wits -f -l branch -s b -r -a '(__wits_git_branches)'

# Worktrees: `create`/`switch` take a REV that must be a local branch
# (--detach widens it to any revision; not worth a conditional), and
# `info`/`prune` accept a TARGET named by branch, path, or directory name.
# DIR is completed only from its position — the fourth token onward, after
# REV is already given.
function __wits_worktree_dirs
    git worktree list --porcelain 2>/dev/null \
        | string match 'worktree *' | string replace 'worktree ' '' | path basename
end
complete -c wits -f -n '__fish_seen_subcommand_from create switch' -a '(__wits_git_branches)'
complete -c wits -f -n '__fish_seen_subcommand_from info prune' -a '(__wits_git_branches)'
complete -c wits -f -n '__fish_seen_subcommand_from info prune' -a '(__wits_worktree_dirs)'
# switch's TARGET (which worktree to move) and create's DIR sit one token
# past REV; the count is an approximation that over-offers when global flags
# precede the position — cosmetic, and the alternative is fragile token
# arithmetic over option values.
complete -c wits -f -n '__fish_seen_subcommand_from switch; and test (count (commandline -opc)) -ge 4' -a '(__wits_worktree_dirs)'
complete -c wits -f -n '__fish_seen_subcommand_from create; and test (count (commandline -opc)) -ge 4' -a '(__fish_complete_directories)'

# MR numbers: read straight from the review store, whose layout —
# <base>/<host>/<owner…possibly nested>/<repo>/<id>/info.json — is pinned by
# wits/docs/reference/review-store.rst. Owner nesting makes the depth
# variable, so entries are found by their marker file rather than position.
# Rung 3 of the store ladder (the common git dir) needs a repository context
# and cannot be resolved from an arbitrary cwd, so only the WITS_REVIEW_DIR
# and XDG_STATE_HOME rungs are honoured here. The description is the MR's
# repo subtree.
function __wits_review_mrs
    set -l base
    if set -q WITS_REVIEW_DIR
        set base $WITS_REVIEW_DIR
    else if set -q XDG_STATE_HOME
        set base $XDG_STATE_HOME/wits/review
    else
        return
    end
    test -d "$base"; or return
    find "$base" -name info.json 2>/dev/null | while read -l info
        set -l mrd (path dirname "$info")
        printf '%s\t%s\n' (path basename "$mrd") (string replace "$base/" '' -- (path dirname "$mrd"))
    end
end
complete -c wits -f -n '__fish_seen_subcommand_from review; and __fish_seen_subcommand_from fetch show diff draft submit checkout prune' -a '(__wits_review_mrs)'

# Feeds: one global TOML file (wits/docs/commands/review.rst) declares one
# [repo."host/…"] section per repo, each feed a `feed.<name> = { … }` key;
# the names are what --feed takes. The path ladder mirrors
# crates/wits/src/cmd/review/config.rs: $WITS_REVIEW_CONFIG, then
# $XDG_CONFIG_HOME/wits/review.toml, then $HOME/.config/wits/review.toml.
function __wits_review_feeds
    set -l cfg ~/.config/wits/review.toml
    if set -q WITS_REVIEW_CONFIG
        set cfg $WITS_REVIEW_CONFIG
    else if set -q XDG_CONFIG_HOME
        set cfg $XDG_CONFIG_HOME/wits/review.toml
    end
    test -f "$cfg"; or return
    set -l repo
    while read -l row
        if set -l m (string match -r '^\s*\[repo\."([^"]+)"\]' -- $row)
            set repo $m[2]
        else if set -l m (string match -r '^\s*feed\.([\w-]+)\s*=' -- $row)
            printf '%s\t%s\n' $m[2] $repo
        end
    end <$cfg
end
complete -c wits -f -l feed -r -a '(__wits_review_feeds)'

# Toolchains and presets: declared in the project config tree; section
# headers are the documented config surface (wits/docs/commands/project.rst),
# so names are read straight out of them. The directory ladder mirrors
# wits-util's resolver exactly (crates/wits-util/src/project/workspace.rs):
# $WITS_PROJECT_CONFIG, then $XDG_CONFIG_HOME/wits/project, then
# $HOME/.wits/project. Presets here are [project.presets.*] only — an
# [org.presets.*] entry additionally needs its org qualifier, which is left
# to typing.
function __wits_project_dir
    if set -q WITS_PROJECT_CONFIG
        echo $WITS_PROJECT_CONFIG
    else if set -q XDG_CONFIG_HOME
        echo $XDG_CONFIG_HOME/wits/project
    else
        echo ~/.wits/project
    end
end
function __wits_toolchains
    set -l dir (__wits_project_dir)
    grep -hoE '^[[:space:]]*\[toolchains\.("[^"]+"|[^].]+)' $dir/*.toml 2>/dev/null \
        | string replace -r '^[[:space:]]*\[toolchains\.("[^"]+"|[^].]+).*' '$1' \
        | string trim --chars='"' | sort -u
end
function __wits_presets
    set -l dir (__wits_project_dir)
    grep -hoE '^[[:space:]]*\[project\.presets\.("[^"]+"|[^].]+)' $dir/*.toml 2>/dev/null \
        | string replace -r '^[[:space:]]*\[project\.presets\.("[^"]+"|[^].]+).*' '$1' \
        | string trim --chars='"' | sort -u
end
complete -c wits -f -l toolchain -s T -r -a '(__wits_toolchains)'
complete -c wits -f -l preset -s p -r -a '(__wits_presets)'

# Build types are meson-aligned, lowercase
# (wits/docs/reference/project-reference.rst).
complete -c wits -f -l build-type -s B -r -a 'plain debug debugoptimized release minsize'

# File arguments: the clap tree carries no ValueHint for these, so the
# generated script offers nothing for them.
complete -c wits -n '__fish_seen_subcommand_from transcrypt; and __fish_seen_subcommand_from clean smudge textconv' -F
complete -c wits -l config -r -F
