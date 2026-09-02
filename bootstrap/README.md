# bootstrap

Bring a machine to the state its registry entry describes.

```sh
bootstrap/bootstrap.sh apply -n khronos3d    # say what it would do
bootstrap/bootstrap.sh apply khronos3d       # do it
```

## The constraint everything is shaped by

**Nothing here may depend on `wits`.** `wits` is something bootstrap *builds*,
from a C++ toolchain and a Rust toolchain that bootstrap installs. An engine
written in `wits` would need `wits` before step one.

So the engine is POSIX shell, and the declarative files are `key: value`
rather than TOML — a format one `awk` pass can read on a machine that has
nothing else yet.

The constraint runs one way only. *Validation* could depend on `wits` some
day, because validation runs on a machine that is already set up. *Execution*
cannot, ever.

## The mental model in one minute

- A **unit** is one thing to do, at `units/<id>/`. It owns a `unit` metadata
  file and a payload.
- A **capability** is something a machine *chose* to be: `workstation`,
  `develop`, `desktop`, `kde`, `virtualization`, `gpu-graphics`,
  `gpu-compute`, `web-server`, `vps-hygiene`. Its registry entry declares
  them.
- A **fact** is something *detected* about the hardware: `gpu:amd`,
  `cpu:intel`, `hw:laptop`, `vps:aws`. A unit's `when` asks about them.
- A **target** is one machine, at `registry/<name>`: which capabilities it
  has, what it should be called, which dotfiles overlay it wants.
- **`order`** states the sequence outright. Nothing derives it.

A unit runs when its capabilities are declared *and* its facts hold *and* it
has a payload for this platform.

`workstation` is worth naming separately from `develop` and `desktop`: it is
the baseline a machine someone sits at wants — its package sources, its shell
and fonts, a swapfile — and a rented host must not inherit it. A server has
its own baseline under `web-server`.

What belongs in neither is what every machine needs whatever it is for.
Refreshing the package index is `package-index`, gated on no capability at
all: it was once a step inside `repos`, and gating it on `workstation` along
with the mirror choice left a rented host installing against whatever index
its provider's image happened to carry.

### Why choices and facts are separate

This is the distinction the whole model rests on, and getting it wrong is what
broke the old tree.

A desktop environment cannot be *detected* before it is installed. The old
runner gated desktop packages on `de:any`, evaluated once before anything ran,
so bootstrapping from a TTY or over SSH found no desktop, skipped every
desktop unit, and reported success. A GPU, conversely, cannot be *declared*
into existence.

So: choices are declared in the registry, facts are detected. `de:` and `os:`
are gone from the vocabulary — the first because it was never a fact, the
second because a payload's filename suffix already says which platform it is
for.

## The command line

```
bootstrap.sh apply [-n] <target>
bootstrap.sh apply [-n] --capabilities <a,b,c>
bootstrap.sh status
```

| Option | Meaning |
|---|---|
| `-n`, `--dry-run` | Resolve and report. Runs nothing, installs nothing, records nothing. |
| `--capabilities` | A capability set given directly, for a machine not worth registering. |
| `--force` | Ignore every state record. |
| `--force-unit <id>` | Ignore one unit's record. Repeatable. |

`ROOT_PASSPHRASE` is required when any selected unit needs root and the run is
unprivileged. It is checked before any work starts, and not asked for at all
when nothing selected needs privilege.

Everything else a run has to be given is declared by the unit that reads it,
and checked the same way — see [What a run must be given](#what-a-run-must-be-given).

### Why the target is named rather than detected

Guessing which machine this is from its hostname is circular: a machine that
has never been bootstrapped carries whatever name its installer chose, and
setting the hostname is one of the things bootstrap does. The old code spent
seventy-five lines on a four-level fallback for this. Now the target names the
entry and the entry says what the machine should become.

An unknown target prints the known ones. It never guesses.

## Adding things

**A package.** Edit `units/<id>/packages.<platform>`. One name per line, `#`
for a comment, so `git blame` points at a single package.

**A distribution.** Add `packages.<distro>` and `run.<distro>` files. Nothing
is inherited — Ubuntu does not fall back to Debian's list — so a derivative
gets its own files and gets nothing for free. That is the trade: adding a
platform is mechanical, and no list is quietly incomplete because it was
leaning on another.

**A machine.** Add a file to `registry/`.

**A unit.** Create `units/<id>/` with a `unit` file and a payload, then add
the id to `order` where it belongs. Both halves are checked: an id in `order`
with no directory is an error, and a directory `order` does not list is an
error. Neither can rot silently.

**An input.** Add the name to `units/<id>/env`, beside the script that reads
it. A name documented anywhere else is a name nobody is told about at the one
moment it matters.

## Unit metadata

```
kind: packages | action
```

| Key | Meaning |
|---|---|
| `kind` | `packages` (a selector list) or `action` (a shell script). |
| `manager` | Which installer a `packages` unit's names are for. Defaults to the platform's system manager; `pipx`, `cargo` and `flatpak` are the others. |
| `capabilities` | Comma-separated. The machine must declare at least one. Empty means "whenever this machine is bootstrapped at all". |
| `requires` | Comma-separated unit ids, for failure propagation and reporting. **Not** for ordering. |
| `when` | Comma-separated hardware facts, all of which must hold. |
| `optional` | Commands whose absence makes the unit not-applicable rather than failed. |
| `root` | `yes` — the unit needs privilege. |
| `unless` | A shell fragment; the unit is already satisfied when it succeeds. |

A misspelled key is an error, not an absent one.

### `requires` does not order anything

`order` is the ordering statement. `requires` exists so that when a unit
fails, the run can say what it took down:

```
[bootstrap] failed:
  a: exit 7
[bootstrap] blocked by the above:
  b (needs a)
  c (needs b)
```

Because `order` is topological by construction, one forward pass gets this
transitively right — no sort algorithm. And because writing an order by hand
makes one specific mistake possible, that mistake is checked: a unit requiring
something listed *below* it fails the run.

### `requires`, `optional`, and neither

Three different questions, which the old `dep:` tag conflated into one silent
skip:

- **`requires`** — ordering and blame. If it failed, this is blocked.
- **`optional`** — this machine legitimately lacks the subsystem.
  `loginctl` on a machine without systemd. Not-applicable, not a failure.
- **Neither** — the tool should be there and its absence is a bug. The unit
  fails loudly.

`optional` is evaluated immediately before the unit runs, not during
selection, because the answer changes within a run: `deno-apps` needs a `deno`
that `node-toolchain` installs a few units earlier.

## What a run must be given

A unit that reads something out of the environment declares it at
`units/<id>/env`, one `NAME: description` per line, in the same `key: value`
format as everything else here.

```
VPS_DOMAIN_NAME: the name the certificate is issued for
DNS_PROVIDER?: set to issue a wildcard over DNS-01; unset means HTTP-01
WITS_TRANSCRYPT_*_PASSWORD: one per transcrypt context this run should decrypt
```

| Shape | Meaning |
|---|---|
| `NAME` | Required. The run refuses to start without it. |
| `NAME?` | Optional. Its absence is legal, and the description says what it means. |
| A name containing `*` | A family the unit discovers for itself. Printed, never checked. |

Only a fixed name can be checked, for the reason only an exact package
selector can be diffed against the installed set. `dotfiles` derives its
transcrypt contexts from whichever passwords the environment carries, so
which members of that family a run needs is the unit's question rather than
the engine's.

Required names are established before any unit runs, and all of them at once:
failing on a missing token twenty minutes in wastes the twenty minutes, and
fixing one only to rediscover the next wastes it again. A unit the engine
would report `cached` is not asked — its work is recorded as done, and
demanding the token that produced it would make an idempotent re-run harder
than the first run was.

Because the check runs before any probe does, a name is declared *required*
only when the unit needs it whenever it runs at all. `certbot-dns` runs only
where `DNS_PROVIDER` is set, and a host doing HTTP-01 must not be asked for a
token it has no use for, so both of its names are optional and "a provider
without a token is an error" stays in its script.

`apply -n <target>` prints the whole contract for one target and says which
names are set. A real run prints the optional ones it was not given, because
"no wildcard, apex only" is worth knowing before the work rather than after
it. Values are never printed; `DNS_API_TOKEN` is a secret.

This is the opposite direction from the `BOOTSTRAP_*` variables at the end of
this file: those the engine computes and exports, these the caller supplies.
`ROOT_PASSPHRASE` is not declared here because it belongs to the engine rather
than to any unit.

## Privilege

There are no planes. A run is one pass, by one user, covering both system and
user work. A unit says `root: yes` and the engine escalates for that unit
alone — no partitioning, no second invocation, no separate state.

Because the whole unit is escalated, **a `root: yes` script contains no
`sudo`**. And because it sees root as its own identity, a unit that modifies
the invoking user reads `BOOTSTRAP_USER`, which the engine captured before
escalating.

The engine escalates and never de-escalates. Nothing today asks to run as a
named non-root user.

## Re-entry

Re-running is the designed path, not a recovery hack.

Almost every unit here is idempotent already — which is why re-running a
broken bootstrap has always happened to work. So state is not what makes
re-entry *correct*; it is what makes it *cheap*. A forty-minute run that died
at ninety percent should cost four minutes the second time.

Three mechanisms, in order of preference:

1. **`kind: packages` asks the manager.** The installed set is read once per
   run and diffed against the selector list. This is not a probe anyone wrote
   — it falls out of having to know what to install. It also makes failure
   isolation free: a batch that fails is retried one selector at a time, so a
   single bad name is reported as one bad name rather than blamed on the
   ninety packages beside it.
2. **`unless` asks the machine.** For actions with a cheap probe. A record can
   be stale, can be lost, and cannot see a change somebody made by hand; a
   probe cannot.
3. **The state record**, at `$XDG_STATE_HOME/wits/bootstrap/<id>`, holding a
   digest of the unit's content. Only for what is genuinely undecidable —
   `zypper dup` has no "already done", a meson build has no cheap probe.
   Editing a unit invalidates its record.

**A unit with an `unless` never consults state.** State is a last resort, not
a source of truth.

**A unit that must run every time says `unless: false`.** `package-index` has
nothing honest to probe — a current index is what it does, and any cheaper
answer is a freshness window nobody chose — while an action unit with *no*
probe is state-cached after its first success.

### What a run reports

| Status | Meaning |
|---|---|
| `ran` / `plan` | Did the work (or, under `-n`, would). |
| `satisfied` | The machine already shows it. Asked, not assumed. |
| `cached` | A state record matches this content. |
| `skip` | Not applicable, **with the reason**. |
| `failed` | Ran and failed, with what failed. |
| `blocked` | Something it requires failed. |

Every skip carries its reason. The old runner printed nothing at all for a
skip, so "not for this OS", "tool missing", and "typo in a tag" were
indistinguishable from each other and from a bug.

### One limitation worth knowing

`apply -n` reports against the machine's *current* state. It cannot see the
effect of units it did not run, so on a fresh machine it under-reports: a unit
whose `optional` tool an earlier unit is about to install shows as skipped.

## Files

```
bootstrap.sh          entry point: CLI, selection, failure propagation, report
order                 the sequence, stated
lib/meta.sh           the `key: value` parser — the one file format
lib/unit.sh           unit metadata, validation, platform resolution, selection
lib/env.sh            the environment a unit declares, and the gate for it
lib/registry.sh       the machine registry
lib/packages.sh       manager dispatch, installed-set cache, failure isolation
lib/privilege.sh      per-unit escalation and the dry-run gate
lib/state.sh          state records
registry/<target>     one file per machine
units/<id>/unit       metadata
units/<id>/env        the environment this unit reads, if any
units/<id>/packages[.<platform>]
units/<id>/run[.<platform>]
```

`scripts/detect.sh` and `scripts/detect_vps.sh` are shared with
`services/runner.sh` and supply the facts. `scripts/tags.sh` and
`scripts/constraints.sh` are **not** used here — those belong to the service
runner, which still selects on file tags.

## What a unit's script may rely on

| Variable | |
|---|---|
| `BOOTSTRAP_ROOT` | The repository root. |
| `BOOTSTRAP_SCRIPTS` | `scripts/`, for sourcing `detect.sh`. |
| `BOOTSTRAP_USER` | The invoking user, captured before escalation. |
| `BOOTSTRAP_HOSTNAME` | What the target says this machine is called. |
| `BOOTSTRAP_PROFILE` | The dotfiles overlay this machine wants. |
| `BOOTSTRAP_CAPABILITIES` | Space separated. |
| `BOOTSTRAP_OS`, `BOOTSTRAP_DISTRO` | |
| `BOOTSTRAP_GPUS`, `BOOTSTRAP_CPU` | |

`PATH` already carries `~/.local/bin`. Everything else comes from the ambient
environment, which `sudo -E` carries across escalation — that is how a unit
sees `DNS_API_TOKEN`, `VPS_DOMAIN_NAME` or the transcrypt passwords, and each
of those is declared in the `env` file of the unit that reads it.

Scripts are invoked with `sh -eu` whatever they set themselves.
