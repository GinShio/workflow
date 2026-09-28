# services

Run the recurring and login-time work each machine should do.

```sh
services/runner.sh -n nightly    # say what would run, and why the rest would not
services/runner.sh nightly       # run it
```

The two types are the two triggers dotfiles deploys as systemd user units:
`nightly-script.timer` starts the service that runs `nightly`, and
`develop-autostart.service` runs `autostart`.

## The mental model in one minute

- A **unit** is one thing to do, at `units/<id>/`. It owns a `unit` metadata
  file and a payload per platform. A unit is the same on every machine.
- A **fact** is something *detected*: the GPU vendor, being a laptop, being on
  AC, the platform, a command being installed.
- A **declaration** is something a machine *chose*: which projects it builds,
  whether it runs an opt-in unit, how often it upgrades. Dotfiles deploys them
  per overlay and per host, so adding a machine or a purpose is adding data,
  not editing a unit.
- **`order`** states the sequence. Nothing derives it.

A unit runs under its trigger when it is enabled, its facts hold, it has a
payload for this platform, its optional commands are present, the power state
allows it, and it is due.

## Unit metadata

```
runs: nightly
every: 5d
```

| Key | Meaning |
|---|---|
| `runs` | Required. The trigger: `autostart` or `nightly`. |
| `every` | Run at most this often: `daily`, `weekly`, `monthly`, `<N>d`, `<N>h` or `<N>m`. |
| `power` | `ac` — skip while on battery. |
| `when` | Comma-separated detected facts, all of which must hold: `gpu:any`, `gpu:<vendor>`, `hw:laptop`. |
| `optional` | Commands whose absence makes the unit not applicable here. |
| `enable` | `no` for a unit a machine has to opt into. Defaults to `yes`. |
| `params` | Comma-separated names a declaration may set for this unit. |

A misspelled key, an unknown fact, a malformed value or a unit `order` does
not list fails the run before anything executes: each of them would otherwise
be a unit that silently never runs.

**`optional` versus a plain dependency.** A command the machine legitimately
may not have — Flatpak, Docker — goes in `optional`, and its absence is a
skip. A command every machine running the services has — wits, tmux, sudo, the
platform's package manager — is not listed: its absence means the machine is
broken, and the unit fails loudly.

**`every`** is measured between run starts: a run judges every unit at the
moment it started and, only when the payload succeeds, records that moment at
`$XDG_STATE_HOME/wits/services/last-run/<id>`, so the time the units
before it take never moves a unit's schedule. In days it counts UTC midnights
crossed rather than elapsed hours, so a timer that fires a few seconds early
is still a day later; `<N>h` and `<N>m` are elapsed time. The record is keyed
by the unit id, so renaming a unit directory starts its schedule over.

## Payloads

`run.<distro>`, `run.<os>` or `run`, resolved the way bootstrap resolves its
payloads: the distro file, then the OS file, then the generic one. Nothing is
inherited — Ubuntu does not run `run.debian`.

Payloads run as `sh -eu`, one child process per unit, and may rely on:

| Variable | |
|---|---|
| `SERVICES_ROOT` | The repository root, for sourcing `scripts/`. |
| `SERVICES_PARAM_<NAME>` | Each name in `params`, upper-cased with `-` as `_`. Space-separated words; empty when nothing declares it. A unit sees only its own. |
| `DOTFILES_OVERLAYS` | The overlays this machine deploys, colon-separated. |

Everything else comes from the ambient environment, including `SUDO_ASKPASS`
from the `~/.config/wits/.env` dotfiles deploys.

## Declarations

Declarations live in `~/.config/wits/services/`, deployed by `dotfiles/wits`
beside the environment file the runner already reads from there, and are read
in this order:

1. `<overlay>.conf` for each overlay in `DOTFILES_OVERLAYS`, in that order —
   the source is `dotfiles/wits/<overlay>/services/<overlay>.conf`.
2. `host.conf`, rendered from the host's `services_host` table in
   `dotfiles/dotdrop/hosts.toml`.

They are plain text in every overlay: `dotfiles/wits/.gitattributes` exempts
`<overlay>/services/` from the encryption the rest of each overlay gets.

Each line is `<unit>.<key>: <value>`, in the same `key: value` format as the
unit files. Comments are whole lines: a `#` after a value is part of it.

| Form | Effect |
|---|---|
| `<unit>.<key>: <value>` | Replace what earlier layers set. |
| `<unit>.<param>+: <words>` | Append to what earlier layers set. Parameters only. |

A declaration may set `enable`, `every`, `power`, and the unit's own
parameters; everything else a unit says about itself is the same everywhere.
An empty `every` or `power` removes that condition. A parameter starts empty,
so a unit with nothing declared for it does nothing with it.

Appending exists because purposes stack: the shared overlay names what every
machine builds, and an overlay for one purpose adds to it. Replacing is how a
later layer states the exact value, so the machine itself always has the last
word.

```
# common.conf
update-projects.projects: mesa spirv-headers llvm

# <overlay>.conf, for machines that also build one more project
update-projects.projects+: <project>
```

```toml
# dotfiles/dotdrop/hosts.toml, for one machine that differs
[hosts.'<hostname>'.variables.services_host.pkg-upgrade]
every = '7d'
```

Two constraints shape the layers:

- **Overlay files are found through the overlay list, not a glob.** Taking an
  overlay off a machine does not delete the file it deployed; reading by glob
  would go on applying it.
- **The host layer is its own top-level variable.** Dotdrop layers a host's
  variables over the shared ones by whole top-level key, so a host writing
  into a shared table would erase the rest of that table. No overlay may be
  named `host`, since its file would be `host.conf`.

As with unit metadata, an unknown unit, an unknown key, a malformed value or
an append to a key that takes one value fails the run before anything
executes. A misspelled `<unit>.enable: no` that was merely ignored would leave
running the very unit it was written to stop.

### The parameters that exist

| Unit | Parameter | Meaning |
|---|---|---|
| `update-projects` | `projects` | Updated, then built in release and debug, after the proxy is set. |
| | `direct` | The same, before the proxy is set. |
| | `install` | Of those, the ones whose release build is installed to its own install directory. |
| | `install-local` | Of those, the ones whose release build is installed into `~/.local`; it wins over `install`. |
| `gpu-test-run` | `drivers` | `<driver>,<suite>` pairs to test. Each is tested only when its driver's libraries differ from the ones it was last tested with. |

## What a run reports

Every unit of the trigger gets one line — `ran` (or `plan` under `-n`),
`skip` with the reason, or `failed` with the exit status — and every parameter
or overridden key of a unit that runs gets a line with its value and the layers
it came from:

```
  update-projects    plan
                             projects = mesa spirv-headers llvm <project> (common, <overlay>+)
  flatpak-update     skip    not due, last run 2026-01-01 05:32:16, every 14d
```

`(common, <overlay>+)` reads: set by `common`, appended to by `<overlay>`. A run
with any failure exits 1.

## Adding things

**A unit.** Create `units/<id>/` with a `unit` file and a payload, then add the
id to `order` where it belongs.

**A platform.** Add `run.<distro>` or `run.<os>` to the units that need it.

**A purpose.** Add `dotfiles/wits/<overlay>/services/<overlay>.conf`
with what machines of that purpose do differently.

**A machine that differs.** Add its `services_host` table to its entry in
`dotfiles/dotdrop/hosts.toml`.

## Files

```
runner.sh             entry point: validation, declarations, selection, report
order                 the sequence, stated
units/<id>/unit       metadata
units/<id>/run[.<platform>]
```

`scripts/meta.sh`, the `key: value` parser, and `scripts/detect.sh`, which
supplies the facts, are shared with bootstrap.
