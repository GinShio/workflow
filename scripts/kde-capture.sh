#!/bin/sh
# Capture the live KDE configuration into the `kde` dotfiles module.
#
# KDE offers no export. It rewrites these files continuously from a running
# session, so the repository copy has to be *taken* from a live machine rather
# than authored, and taken again whenever the settings change.
#
# What makes that safe is the KConfig cascade: /etc/xdg, then $XDG_CONFIG_DIRS,
# then ~/.config/kdedefaults, then ~/.config. A key absent from the top layer
# falls back to its upstream default rather than to nothing, so a file with the
# private parts cut out is a *working* file, not a broken one. That is the whole
# reason "commit the preferences, drop the identity" is possible here.
#
# Run it after changing settings, then read `git diff dotfiles/kde` before
# committing. Nothing is deployed; deployment stays with dotdrop.
#
# One limitation to know: capture overwrites, so hand-written Jinja in a
# captured file does not survive. The $HOME rewrite below is the only
# templating these files carry, and it is reapplied on every capture.

set -eu

REPO=$(cd -- "$(dirname -- "$0")/.." && pwd)
MODULE="$REPO/dotfiles/kde/common"

# Every file the module owns, as a path relative to the module's `common/`
# tree. One per line so `git blame` points at a single file, and so adding or
# dropping one is a one-line change.
#
# What is deliberately absent, and why:
#
#   kwinoutputconfig.json      monitor EDID and serial numbers
#   kactivitymanagerdrc        this machine's activity UUID
#   ksmserverrc                the last session's restored applications
#   kconf_updaterc             which migration scripts have run here
#   trashrc                    mounted network share hostnames
#   konsolesshconfig, krdcrc   remote host inventory
#   bluedevilglobalrc          the Bluetooth adapter's MAC address
#   knighttimerc               latitude and longitude
#   ktimezonedrc               the timezone, for the same reason
#   emailidentities, emaildefaults, akregatorrc, kontactrc, kleopatrarc
#                              account identities
#   kdeconnect/                paired device certificates
#   kmozillahelperrc, phishingurlrc, katemetainfos
#                              recently-opened file paths
#   systemmonitorrc, khelpcenterrc, kgammarc, powermanagementprofilesrc
#                              window geometry or migration markers only
FILES='
config/kdeglobals
config/plasmarc
config/ksplashrc
config/kcmfonts
config/kcminputrc
config/kxkbrc
config/plasma-localerc
config/plasma_calendar_alternatecalendar
config/plasma_calendar_holiday_regions

config/kwinrc
config/kglobalshortcutsrc

config/plasma-org.kde.plasma.desktop-appletsrc
config/plasmashellrc
config/plasmanotifyrc
config/krunnerrc
config/kactivitymanagerd-statsrc

config/baloofilerc
config/baloofileinformationrc

config/kscreenlockerrc
config/powerdevilrc
config/kded5rc
config/kded6rc
config/kiorc
config/kwalletrc

config/mimeapps.list
config/filetypesrc
config/dolphinrc
config/konsolerc
config/spectaclerc
config/okularrc
config/okularpartrc
config/gwenviewrc
config/arkrc
config/filelightrc
config/discoverrc
config/drkonqirc

kdedefaults/package
kdedefaults/kdeglobals
kdedefaults/kcminputrc
kdedefaults/ksplashrc
kdedefaults/kwinrc
kdedefaults/plasmarc

color-schemes/Arc.colors
color-schemes/ArcDark.colors
color-schemes/Dracula.colors
color-schemes/DraculaPurple.colors
color-schemes/Layan.colors
color-schemes/LayanLight.colors
color-schemes/WhiteSur.colors
color-schemes/WhiteSurAlt.colors
color-schemes/WhiteSurDark.colors

konsole/GinShio.profile
'

# What to cut out of a captured file: `<file> section <ere>` drops a whole
# bracketed group, `<file> key <ere>` drops one assignment wherever it appears.
# Both patterns are extended regular expressions rather than globs, because a
# glob-to-regex translator would be the least trustworthy part of this script.
#
# A `key` pattern is matched against the key name with any KConfig modifier
# stripped, so `^exclude folders$` also matches `exclude folders[$e]=`.
STRIP='
# The virtual desktop UUIDs, and the per-output tile layouts keyed by them.
# Number and Rows are the actual preference and stay; KWin mints a fresh Id_N
# for a desktop that has none.
config/kwinrc                                   key      ^Id_[0-9]+$
config/kwinrc                                   section  ^\[Tiling\]

# Holds one entry, a shortcut bound to this machine activity UUID.
config/kglobalshortcutsrc                       section  ^\[ActivityManager\]$

# Per-output scale factors name this machine outputs (DP-1, HDMI-1, ...), and
# the hash is a digest of the live color scheme that Plasma recomputes.
config/kdeglobals                               section  ^\[KScreen\]$
config/kdeglobals                               key      ^ColorSchemeHash$

# The panel layout is worth keeping; where its widgets landed on which screen
# is not, and neither is the size every configuration dialog was left at.
config/plasma-org.kde.plasma.desktop-appletsrc  section  ^\[ScreenMapping\]$
config/plasma-org.kde.plasma.desktop-appletsrc  key      ^ItemGeometries
config/plasma-org.kde.plasma.desktop-appletsrc  key      ^(Dialog|popup)(Height|Width)$
config/plasma-org.kde.plasma.desktop-appletsrc  key      ^history$

# The ledger of which kconf_update scripts have run on this machine.
config/plasmashellrc                            key      ^performed$

# A most-recently-used list of wallpaper files.
config/plasmarc                                 section  ^\[Wallpapers\]$

# The launcher favourites exist twice, once globally and once bound to the
# activity UUID. The global copy is the portable one.
config/kactivitymanagerd-statsrc                section  ^\[Favorites-.*-[0-9a-f]{8}-

# Where the last screenshot and screencast were written.
config/spectaclerc                              section  ^\[(Image|Video)Save\]$

# Recently-opened documents, and window state that would churn every capture.
config/gwenviewrc                               section  ^\[Recent Files\]$
config/gwenviewrc                               key      ^(.*SplitterSizes|PrintHeight|LastUsedVersion)$
config/okularrc                                 section  ^\[Recent Files\]$
config/arkrc                                    key      ^(DirHistory|splitterSizes)$
config/dolphinrc                                key      ^ViewPropsTimestamp$

# Baloo stamps its own index format version.
config/baloofilerc                              key      ^dbVersion$
'

# Files copied byte for byte, because they are not the `key=value` format the
# filter below parses and running them through it would reformat them.
# `kdedefaults/package` holds a bare package id with no trailing newline, and
# nothing here can promise the reader trims one.
VERBATIM='
kdedefaults/package
'

# Every list below reaches awk through the environment rather than through
# `-v`, which would process the backslashes in the patterns above as escape
# sequences and quietly turn `\[Tiling\]` into `[Tiling]`.
export FILES STRIP VERBATIM

# Where a module-relative path comes from on a live machine. The `config/`
# prefix exists because those files sit flat in ~/.config while the other three
# trees are real directories with names of their own.
live_path() {
    case "$1" in
        config/*)      printf '%s/.config/%s\n' "$HOME" "${1#config/}" ;;
        kdedefaults/*) printf '%s/.config/%s\n' "$HOME" "$1" ;;
        *)             printf '%s/.local/share/%s\n' "$HOME" "$1" ;;
    esac
}

# The rules of one kind for one file, joined into a single alternation so the
# filter runs in one pass. Empty when the file has none.
rules_for() {
    RULE_FILE=$1 RULE_KIND=$2 awk '
        BEGIN {
            n = split(ENVIRON["STRIP"], lines, "\n")
            for (i = 1; i <= n; i++) {
                if (lines[i] ~ /^[ \t]*(#|$)/) continue
                if (split(lines[i], f, /[ \t]+/) < 3) continue
                if (f[1] != ENVIRON["RULE_FILE"]) continue
                if (f[2] != ENVIRON["RULE_KIND"]) continue
                pattern = lines[i]
                sub(/^[ \t]*[^ \t]+[ \t]+[^ \t]+[ \t]+/, "", pattern)
                out = (out == "" ? pattern : out "|" pattern)
            }
            if (out != "") print "(" out ")"
        }'
}

# Copy one file, minus its rules, rewriting this user home directory into the
# Dotdrop variable so the result deploys anywhere.
#
# A section whose every key was dropped is dropped with it, and blank lines are
# regenerated rather than carried, so removing a section's last key does not
# leave a header and a hole behind.
capture_file() {
    SECTIONS=$3 KEYS=$4 awk -v home="$HOME/" -v token="{{@@ env['HOME'] @@}}/" '
        function unhome(s,   p, out) {
            while ((p = index(s, home)) > 0) {
                out = out substr(s, 1, p - 1) token
                s = substr(s, p + length(home))
            }
            return out s
        }
        BEGIN { sections = ENVIRON["SECTIONS"]; keys = ENVIRON["KEYS"] }
        /^\[/ {
            dropping = (sections != "" && $0 ~ sections)
            header = $0
            shown = 0
            next
        }
        dropping { next }
        $0 == "" { next }
        {
            if (keys != "") {
                eq = index($0, "=")
                if (eq > 0) {
                    name = substr($0, 1, eq - 1)
                    sub(/\[[^]]*\]$/, "", name)
                    if (name ~ keys) next
                }
            }
            if (!shown) {
                if (header != "") {
                    if (printed) print ""
                    print unhome(header)
                }
                shown = 1
                printed = 1
            }
            print unhome($0)
        }' "$1" >"$2"
}

# Advisory only: the redaction rules are the mechanism, this is the check that
# they still cover what arrives. UUIDs are not flagged, because the panel
# layout keeps one on purpose.
audit() {
    grep -rEn \
        -e '/home/' \
        -e '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' \
        -e '([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}' \
        -e '(smb|ssh|sftp|nfs)://' \
        "$MODULE" 2>/dev/null || true
}

is_verbatim() {
    printf '%s\n' "$VERBATIM" | grep -qxF -- "$1"
}

# A rule that names a file the module does not capture, or one it copies
# verbatim, does nothing at all — and does it silently. Catching the typo is the
# only thing keeping these three lists in agreement with each other.
unknown=$(awk '
    BEGIN {
        n = split(ENVIRON["FILES"], f, "\n")
        for (i = 1; i <= n; i++) if (f[i] != "") owned[f[i]] = 1
        n = split(ENVIRON["VERBATIM"], v, "\n")
        for (i = 1; i <= n; i++) if (v[i] != "") copied[v[i]] = 1
        n = split(ENVIRON["STRIP"], s, "\n")
        for (i = 1; i <= n; i++) {
            if (s[i] ~ /^[ \t]*(#|$)/) continue
            split(s[i], p, /[ \t]+/)
            if (!(p[1] in owned)) print p[1] " is not captured"
            else if (p[1] in copied) print p[1] " is copied verbatim"
        }
    }')
if [ -n "$unknown" ]; then
    printf 'Strip rules that would do nothing:\n%s\n' "$unknown" >&2
    exit 1
fi

captured=0
missing=''
for entry in $FILES; do
    src=$(live_path "$entry")
    if [ ! -f "$src" ]; then
        missing="$missing $entry"
        continue
    fi

    dst="$MODULE/$entry"
    mkdir -p -- "$(dirname -- "$dst")"
    if is_verbatim "$entry"; then
        cp -- "$src" "$dst"
    else
        capture_file "$src" "$dst" \
            "$(rules_for "$entry" section)" "$(rules_for "$entry" key)"
    fi
    captured=$((captured + 1))
done

printf 'Captured %d files into %s\n' "$captured" "${MODULE#"$REPO"/}"

# Absent is not an error: a file KDE has not written yet is a setting left at
# its default, and on a machine that never opened Ark there is no arkrc.
if [ -n "$missing" ]; then
    printf 'Not present on this machine, skipped:%s\n' "$missing"
fi

leaks=$(audit)
if [ -n "$leaks" ]; then
    printf '\nReview these before committing:\n%s\n' "$leaks" >&2
fi
