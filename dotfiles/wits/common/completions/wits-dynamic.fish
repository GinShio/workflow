# wits — dynamic value completion: the parts the generator cannot know.
#
# The static skeleton (subcommands, flags, enum values) is generated from the
# clap tree by `wits __completions fish`, which `meson install` runs to
# install it as <datadir>/fish/vendor_completions.d/wits.fish
# (wits/meson.build), so it is refreshed whenever wits is. This file holds
# only values queried from the world: it changes when a data source moves,
# not when a flag does. Sourcing at startup only registers completions and
# defines functions; every query runs at TAB time.
#
# Every built-in is also an applet (`wits-review`, …): the same command tree
# under another argv[0], which main.rs splices back into the umbrella parse.
# Each rule below is therefore registered for both spellings, and its
# condition asks which *verb* is in play rather than which tokens were seen —
# the verb is the one fact both forms agree on.
#
# Registry-sourced names (toolchains, presets, repos) come from `wits project
# __names`, the hidden query the tool ships for exactly this: completion asks
# the registry instead of keeping a TOML parser and a copy of the config
# ladder, the same bargain `__trunk` made for the git hooks.

# Which wits verb is in play. The applet form carries it in argv[0]
# (`wits-review` → review); the umbrella form spells it as the first word
# after the program that is not a flag — `-v`/`-n` are the only root-global
# flags, and neither takes a value, so nothing can precede the verb but them.
function __wits_verb --description 'the wits verb in play, umbrella or applet form'
    set -l first (path basename (commandline -opc)[1])
    # `wits-*` requires the dash, so the bare umbrella program falls through
    # to the token scan below. (fish's string-match glob has no usable `?`.)
    if string match -q 'wits-*' -- $first
        string replace -r '^wits-' '' -- $first
        return 0
    end
    for tok in (commandline -opc)[2..]
        if string match -q -- '-*' $tok
            continue
        end
        echo $tok
        return 0
    end
    return 1
end

function __wits_ctx --description 'true when the verb in play is one of the named built-ins'
    set -l verb (__wits_verb)
    test -n "$verb"; and contains -- $verb $argv
end

# The project target typed on this line, if any: the first word that is
# neither a flag, a flag's value, nor a verb. A wrong guess is harmless — the
# scoped query exits non-zero and the caller falls back to the union.
function __wits_target
    set -l verbs build devenv update project stack review worktree transcrypt \
        dotfiles list info check exists branch-build-dirs hash
    set -l prev
    for tok in (commandline -opc)[2..]
        if string match -q -- '-*' $tok
            set prev $tok
            continue
        end
        if string match -q -- '-*' $prev
            set prev $tok
            continue
        end
        if contains -- $tok $verbs
            set prev $tok
            continue
        end
        echo $tok
        return
    end
end

# Projects: `wits project list` prints one `org/name focus=… build=…` row per
# project. Both the bare name and org/name are accepted as [NAME|PATH]
# (wits/docs/commands/project.rst), so the candidate is the bare name with
# org/name as the description.
function __wits_projects
    wits project list 2>/dev/null | while read -l row
        set -l orgname (string split -f 1 ' ' -- $row)
        string match -q '*/*' -- $orgname; or continue
        printf '%s\t%s\n' (string split -m 1 -f 2 / -- $orgname) $orgname
    end
end
complete -c wits -c wits-build -c wits-devenv -c wits-update \
    -f -n '__wits_ctx build devenv update' -a '(__wits_projects)'
# Scoped past the verbs: `wits project <TAB>` must offer the verbs, not the
# projects the verbs consume.
complete -c wits -c wits-project \
    -f -n '__wits_ctx project; and __fish_seen_subcommand_from list info check exists branch-build-dirs hash' -a '(__wits_projects)'

# `project info --get` paths: a dotted lookup into the template context
# (wits/docs/reference/project-reference.rst). The namespace is open — `env.*`
# is only known at resolve time — so this offers the roots and the enumerable
# leaves and leaves the rest to typing, the same bargain the toolchain/preset
# completions make. The `repos.<name>.*` entries come from the registry query,
# scoped to the target when one is on the line.
function __wits_project_get_paths
    # Resolved plan outputs and the Profile axes: flat, always present.
    printf '%s\n' source_dir build_dir install_dir build_type generator
    # The focus repo, by its relative alias — `workdir` included, which is how
    # a caller names the focus's checkout without knowing its repo name.
    for f in name path kind main_branch anchor workdir
        printf 'repo.%s\n' $f
    end
    printf '%s\n' project.name project.org project.focus
    printf '%s\n' branch.raw branch.slug
    for f in name cc cxx rustc ar nm ranlib strip linker launcher \
        c_flags cxx_flags link_flags
        printf 'toolchain.%s\n' $f
    end
    printf '%s\n' system.os.name system.cpu.count system.cpu.arch \
        system.mem.gb system.mem.mb
    for repo in (__wits_repos)
        for f in name path kind main_branch anchor workdir
            printf 'repos.%s.%s\n' $repo $f
        end
    end
end
complete -c wits -c wits-project \
    -f -n '__wits_ctx project; and __fish_seen_subcommand_from info' -l get -r -a '(__wits_project_get_paths)'

# Toolchains, presets, repos: one hidden query, three lists. With a target on
# the line the answer is that project's own; a target that does not resolve
# exits non-zero and the fallback below answers for the whole registry — the
# over-approximation worth having when the target is still being typed.
function __wits_toolchains
    wits project __names toolchains 2>/dev/null
end

function __wits_presets
    set -l target (__wits_target)
    if test -n "$target"
        wits project __names presets $target 2>/dev/null
        and return
    end
    wits project __names presets 2>/dev/null
end

function __wits_repos
    set -l target (__wits_target)
    if test -n "$target"
        wits project __names repos $target 2>/dev/null
        and return
    end
    wits project __names repos 2>/dev/null
end

# The profile flags sit after the verb that reads them, and nowhere else
# (wits/docs/reference/project-reference.rst, "CLI"): the whole profile goes
# with a build — `build`, `devenv`, `project info` — while the checkout queries
# `project hash` and `project branch-build-dirs` take only --branch and
# --focus. `update`, `stack`, `review`, `worktree` take no profile.
function __wits_takes_profile --description 'true where the whole build profile is accepted'
    __wits_ctx build devenv; and return 0
    __wits_ctx project; and __fish_seen_subcommand_from info
end

function __wits_takes_branch --description 'true where --branch and --focus are accepted'
    __wits_takes_profile; and return 0
    __wits_ctx project; and __fish_seen_subcommand_from hash branch-build-dirs
end

complete -c wits -c wits-project -c wits-build -c wits-devenv \
    -f -n __wits_takes_profile -l toolchain -s T -r -a '(__wits_toolchains)'
complete -c wits -c wits-project -c wits-build -c wits-devenv \
    -f -n __wits_takes_profile -l preset -s p -r -a '(__wits_presets)'
# `--focus` names a repo of the target, so it takes the same registry list the
# `repos.<name>.*` paths above do.
complete -c wits -c wits-project -c wits-build -c wits-devenv \
    -f -n __wits_takes_branch -l focus -r -a '(__wits_repos)'
# Build types are meson-aligned, lowercase
# (wits/docs/reference/project-reference.rst).
complete -c wits -c wits-project -c wits-build -c wits-devenv \
    -f -n __wits_takes_profile -l build-type -s B -r -a 'plain debug debugoptimized release minsize'

# Branches: local branches of the current repo. Self-contained — fish 4.x
# removed the old __fish_git_branches helper, and depending on which of its
# internal replacements exists would tie this file to one fish version. wits
# ships no branch-listing query; for the stack and worktree verbs the current
# repo is the one in play, and -b/--branch is normally typed inside the focus
# repo — from elsewhere the completion is simply empty rather than wrong.
function __wits_git_branches
    git for-each-ref refs/heads --format='%(refname:short)' 2>/dev/null
end
# The branch-positional stack verbs. `slice` takes no positional — its branch
# is the `--base` flag, which gets its own value rule; `tree` takes branches
# as positionals in both `rm` and `mv` (`edit` takes a file).
complete -c wits -c wits-stack \
    -f -n '__wits_ctx stack; and __fish_seen_subcommand_from status push submit anno publish decorate' -a '(__wits_git_branches)'
complete -c wits -c wits-stack \
    -f -n '__wits_ctx stack; and __fish_seen_subcommand_from slice' -l base -r -a '(__wits_git_branches)'
complete -c wits -c wits-stack \
    -f -n '__wits_ctx stack; and __fish_seen_subcommand_from tree; and __fish_seen_subcommand_from rm mv' -a '(__wits_git_branches)'
complete -c wits -c wits-project -c wits-build -c wits-devenv \
    -f -n __wits_takes_branch -l branch -s b -r -a '(__wits_git_branches)'

# Worktrees: `create`/`switch` take a REV that must be a local branch
# (--detach widens it to any revision; not worth a conditional), and
# `info`/`prune` accept a TARGET named by branch, path, or directory name.
function __wits_worktree_dirs
    git worktree list --porcelain 2>/dev/null \
        | string match 'worktree *' | string replace 'worktree ' '' | path basename
end
complete -c wits -c wits-worktree \
    -f -n '__wits_ctx worktree; and __fish_seen_subcommand_from create switch' -a '(__wits_git_branches)'
complete -c wits -c wits-worktree \
    -f -n '__wits_ctx worktree; and __fish_seen_subcommand_from info prune' -a '(__wits_git_branches)'
complete -c wits -c wits-worktree \
    -f -n '__wits_ctx worktree; and __fish_seen_subcommand_from info prune' -a '(__wits_worktree_dirs)'
# switch's TARGET (which worktree to move) and create's DIR sit one token
# past REV; the count is an approximation that over-offers when global flags
# precede the position — cosmetic, and the alternative is fragile token
# arithmetic over option values. The applet form has one fewer leading token.
complete -c wits \
    -f -n '__wits_ctx worktree; and __fish_seen_subcommand_from switch; and test (count (commandline -opc)) -ge 4' -a '(__wits_worktree_dirs)'
complete -c wits-worktree \
    -f -n '__fish_seen_subcommand_from switch; and test (count (commandline -opc)) -ge 3' -a '(__wits_worktree_dirs)'
complete -c wits \
    -f -n '__wits_ctx worktree; and __fish_seen_subcommand_from create; and test (count (commandline -opc)) -ge 4' -a '(__fish_complete_directories)'
complete -c wits-worktree \
    -f -n '__fish_seen_subcommand_from create; and test (count (commandline -opc)) -ge 3' -a '(__fish_complete_directories)'

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
complete -c wits -c wits-review \
    -f -n '__wits_ctx review; and __fish_seen_subcommand_from fetch show diff draft edit submit checkout prune' -a '(__wits_review_mrs)'

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
# --feed exists only on `review fetch`.
complete -c wits -c wits-review \
    -f -n '__wits_ctx review; and __fish_seen_subcommand_from fetch' -l feed -r -a '(__wits_review_feeds)'

# File arguments: the clap tree carries no ValueHint for these, so the
# generated script offers nothing for them. transcrypt's filter verbs read a
# file; dotfiles' --config/--root (global within the dotfiles tree) name the
# layout declaration and a repository root.
complete -c wits -c wits-transcrypt \
    -n '__wits_ctx transcrypt; and __fish_seen_subcommand_from clean smudge textconv' -F
complete -c wits -c wits-dotfiles \
    -n '__wits_ctx dotfiles' -l config -r -F
complete -c wits -c wits-dotfiles \
    -n '__wits_ctx dotfiles' -l root -r -a '(__fish_complete_directories)'
