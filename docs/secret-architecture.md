# Secret Architecture: Transport, Custody, Consumption

**Status:** design document, pre-implementation. The decision record is §10;
open items are §11. Nothing in this file is secret — it deliberately contains
no credentials, hosts, or private paths beyond patterns already public in
`.gitattributes`.

---

## 1. Summary

Secrets live in three layers, each with one job and one boundary:

```
Layer 1  TRANSPORT   transcrypt filters (existing, unchanged)
                     Private content crosses public forges as ciphertext,
                     forever, in every revision.

Layer 2  CUSTODY     wits agent (new)
                     Every password and credential this repository needs
                     lives in one session-scoped service. Nothing at rest
                     on disk is plaintext.

Layer 3  CONSUMPTION per-tool wiring (new)
                     Each tool receives secrets through the interface shape
                     it actually supports: a signing oracle, an injected
                     string, or — last resort — a contained file.
```

The three layers are clients of each other in one direction only:
consumption asks custody; custody is the only holder; transport is
oblivious to both. The design goal is that **plaintext exists only in
the memory of a process that is actively using it** — never asleep on
disk, never in a second file, never in a config that outlives the
session.

---

## 2. Threat model

### 2.1 What we defend against

| # | Adversary | Example | Defended by |
|---|-----------|---------|-------------|
| A1 | Public forge readers and scrapers | anyone browsing the repositories | Layer 1 (ciphertext only, path-bound AEAD) |
| A2 | Offline cracker with full history | ciphertext + salt ground at leisure, forever | Layer 1 KDF (argon2id) **and** machine-grade password entropy (standing requirement) |
| A3 | Same-UID process on a live machine | any daemon or tool run as the user | *honest limit* — see 2.3; reduced exposure only |
| A4 | Disk, backup, or clone theft | laptop lost, `$HOME` copied | Layers 2+3 (no plaintext at rest to steal) |
| A5 | Future credential leak | one password compromised in year N | Layer 2 rotation runbook; *caveat* — Layer 1 history is decryptable under the leaked password regardless of rotation (2.2) |

### 2.2 Standing assumptions

- **Forge retention.** Every ciphertext ever pushed is retained
  indefinitely by the forges — including after force-push, in forks and
  caches. "Delete from HEAD" is not "delete". Rotation protects the
  future only; it never un-leaks history. All design decisions below
  assume this.
- **Password entropy.** Because A2 is offline and unthrottled, every
  transcrypt context password must be machine-generated (≥ 256 bits of
  entropy) and never reused anywhere else. This is a permanent
  requirement of Layer 1, not a one-time setup.
- **Metadata is public.** Paths, commit messages, sizes, and — because
  the SIV mode is deterministic — *equality* of file contents across
  revisions are visible without the key. Accepted in exchange for a
  filter that does not fight git; documented in the wits crypto module.

### 2.3 Honest limits (what this design does not claim)

- **Plaintext in memory is unavoidable.** Encryption and decryption
  require the key material in RAM while running. The goal is that
  plaintext exists *only* there — never at rest — not that it never
  exists.
- **A same-UID process can reach in-use secrets.** While the user is
  logged in, any process run as the user can connect to the custody
  socket, exactly as it could to `ssh-agent` or a GNOME keyring. This
  design reduces A3 exposure (no disk source, single custodian) but
  cannot eliminate it; that boundary belongs to the OS (sandboxing,
  LSMs), not to this architecture.
- **Editing plaintext is still plaintext.** The working tree of this
  repository holds decrypted content whenever it is being edited. That
  is local disk, covered by the machine's own disk encryption; Layer 1
  protects the *forge*, not the working tree.

---

## 3. Principles

1. **One custodian.** All passwords and credentials live in the custody
   service. Every consumer is a client; nothing reads a key file.
2. **The interface shape decides the mechanism.** Tools that accept a
   signing oracle never need plaintext at all; tools that accept a
   string get it injected; only tools that demand a plaintext file are
   allowed one, contained.
3. **Plaintext has one legal home: active memory.** Anything found
   asleep on disk in plaintext is a bug in this architecture.
4. **Degrade loudly.** A missing credential fails the operation with a
   message naming the missing piece — it never silently falls back to a
   weaker path. (Already the rule in the wits resolver; kept.)
5. **No cross-context bleed.** A context-scoped lookup never falls back
   to the context-less key. (Already the rule; kept.)
6. **Each migration step is independently valuable.** No step depends on
   a later one to be worth doing.

---

## 4. Layer 1 — Transport and history (kept as-is)

**What it is.** The existing wits transcrypt: clean/smudge filters
wired through `.gitattributes`, ChaCha20-Poly1305 AEAD with the file
path as additional data, content-derived salt/IV (SIV mode) for filter
idempotence, argon2id key derivation. Repositories — and the forges
holding them — see only base64 ciphertext packets.

**Why it stays.** Among transparent-filter designs it is already the
strongest shape available: authenticated encryption (no unauthenticated
CBC), a memory-hard KDF (unlike git-crypt or upstream transcrypt), and
path binding (a blob cannot be swapped between paths). Replacing it
would be a downgrade in every axis this document cares about (see
Appendix A).

**What it deliberately does not do.** It does not protect secrets at
rest on the machines (the working tree is plaintext by design), and it
does not manage keys (the password has to come from somewhere — that is
Layer 2's job). It also does not hide metadata (2.2).

**Context to pin.** The determinism trade-off is load-bearing: a
non-deterministic filter would produce phantom diffs on every `git
add`. Anyone touching the SIV mode must re-read the module comment in
`wits-util/src/crypto.rs` before changing it.

---

## 5. Layer 2 — Custody

### 5.1 The custody service

One session-scoped daemon, `wits agent`:

```
socket:   $XDG_RUNTIME_DIR/wits/agent.socket   (0600, unix stream)
lifetime: user session; the runtime dir is tmpfs and dies with the session
state:    unlocked secrets in memory (mlock where available), dropped on
          LOCK, on session end, or when the last client goes away
```

Protocol verbs (length-prefixed frames over the socket):

| Verb | Request | Response | Purpose |
|------|---------|----------|---------|
| `UNLOCK` | context list | ok / error | interactive first unlock of a session |
| `DERIVE` | context, salt, KDF, params | 32-byte key | filters get a *derived key*; the password never leaves the agent |
| `PASSWORD` | context | password string | escape hatch for consumers that need the raw secret (kept minimal) |
| `GET` | credential name | secret string | Layer 3 tool credentials |
| `STATUS` | — | unlocked contexts, sources | audit |
| `LOCK` | — | ok | drop everything |

**The boundary this buys (per SIMPLICITY):** the outside — every
filter, tool, and script — knows exactly one thing: *how to ask the
socket*. Which backend holds the secret, how it was unlocked, whether
it is TPM-sealed or keyring-backed or typed in this morning — all of
that is absorbed inside the agent. Swapping a backend changes no
caller.

### 5.2 Unlock sources (inside the agent)

The agent unlocks a context from the first available source:

1. **TPM-sealed credential file.** The password sealed by
   `systemd-creds encrypt` against this machine's TPM (optionally
   PCR-bound to the boot state). Zero-touch: the agent unseals at first
   use, no prompt. Cost: binding is per-machine; reinstalling or
   changing firmware state requires re-sealing — which is the correct
   semantics for a machine-local custody copy.
2. **libsecret keyring.** Item stored in the login keyring, unlocked by
   PAM at login. Zero-touch on desktops; headless machines need the
   keyring pre-unlocked or fall through.
3. **Interactive passphrase.** pinentry on `UNLOCK`; the human is the
   source. Always available; used for enrollment and recovery.

The sealed file and keyring item hold *the same password* that
encrypted the repository — the custodian changes where it sleeps, not
which secret opens the repo.

### 5.3 Resolver integration (wits side)

The existing `Resolver` precedence chain gains one source:

```
1. environment  (WITS_TRANSCRYPT_*_PASSWORD)   bootstrap / CI — unchanged
2. agent        (DERIVE / PASSWORD verbs)      new — the normal path
3. git config   (wits.transcrypt.<ctx>.password)  legacy — removed last
```

`wits transcrypt status` already reports the source of every value; it
now becomes the standing audit command: *"where did my key come from
this time?"* When step 3 of the migration completes, the git-config
source is deleted from the code path and the password lines are deleted
from `config.d/*.conf` — at rest, no file on disk contains a password.

### 5.4 The DERIVE boundary

The subtle piece, and the reason it exists: **filter processes should
never hold the password.**

A filter invocation today: read packet header (salt, KDF, params) → run
argon2id over the password → AEAD with the derived key. Under the new
boundary the filter parses the same header and asks the agent
`DERIVE(context, salt, kdf, params)` instead. The KDF runs inside the
agent; only the 32-byte derived key crosses the socket.

Consequence: the population of processes that ever see the password
shrinks from *every filter invocation* (git spawns one per file per
operation) to *one agent*. This requires splitting KDF from AEAD in the
crypto path — a small refactor of `crypto.rs`, which already keeps the
two phases separable.

### 5.5 Performance

One argon2id per filter invocation is inherent to the SIV design (salt
is content-derived, so it differs per file — there is nothing to
amortize across files). At 128 MiB × 4 this is on the order of a
hundred milliseconds per file on a desktop CPU; acceptable for config
fragments, and the memory-cost dimension matters more than iteration
count for A2 anyway.

The agent memoizes `(context, salt, kdf, params) → key`, so repeated
operations on the same file (smudge after clean, status refreshes,
textconv) become cache hits. Bootstrap keeps the pure env path and
never needs the agent at all.

---

## 6. Layer 3 — Consumption

### 6.1 Interface shapes

A tool consumes a secret through exactly one of three interfaces, and
the interface — not our preference — decides the best achievable
guarantee:

| Shape | Interface | Best achievable | Example |
|-------|-----------|-----------------|---------|
| Oracle | "sign this challenge" | secret **never** leaves the boundary; no plaintext anywhere, ever | ssh-agent, FIDO2 token |
| String | "give me the value" | sealed at rest, injected at use, plaintext in tool memory while running | API keys, tokens |
| File | "read this path" | resident plaintext, contained (0600, tmpfs where possible) | tools with no other input |

### 6.2 Oracle-shaped: SSH keys

The only secret class in this repository that can reach the oracle
shape. Options, strongest first:

- **FIDO2 resident keys (`ssh-keygen -t ed25519-sk`) — preferred.** The
  private key is generated *inside* the hardware token and never
  exists outside it. The on-disk `*_sk` file is a key handle (public
  part + credential id) — safe to keep, useless without the token.
  `git clone` over ssh works unchanged; each use requires a token
  touch. Enrollment: register two tokens (private keys of this type
  cannot be backed up). This is the only option under which no disk —
  ever, on any machine — has held the private key.
- **Passphrase-protected key + ssh-agent — fallback.** The key file at
  rest is encrypted by the OpenSSH key format's own KDF; `ssh-add`
  loads it once per session; ssh config switches from `IdentityFile`
  to `IdentityAgent` (a one-line change in the existing dotdrop ssh
  template).
- **TPM-held keys (`tpm2-pkcs11` behind ssh-agent).** Same guarantee
  class as FIDO2 without dedicated hardware, at the cost of a more
  fragile operational surface. Noted, not chosen by default.

### 6.3 String-shaped tools

One universal accessor: `wits credential get NAME`, served by the same
agent (`GET` verb). Per tool:

- **pi** — verified native support: `apiKey` values accept
  `!command` (execute and use stdout) and `$ENV_VAR` interpolation in
  the provider configuration. The deployed auth file keeps its
  structure; the key material becomes a command reference into the
  custodian. No key on disk.
- **git over HTTPS** — git has a native credential-helper protocol;
  wire it to the keyring (`git-credential-libsecret`) or to a thin
  `wits` helper over the agent. Either keeps `.git-credentials`-style
  plaintext files out of existence.
- **claude / codex** — verify each tool's native helper mechanism
  (`apiKeyHelper`-style script or env); where none exists, a wrapper
  that injects the variable at exec time. Both variants keep the
  source of truth in the custodian. *(Open item — each tool needs a
  documentation check before wiring.)*

**Deployment consequence.** For this class the dotdrop manifests stop
carrying key material at all: the deployed config holds a *reference*
(`!wits credential get …`), and the render pipeline is no longer a
secret conduit. The encrypted manifests keep protecting whatever still
needs transport, but the number of things that need it shrinks.

### 6.4 File-shaped: containment, not pretense

For tools that accept nothing but a plaintext file, the rules are:

1. `0600`, and on a `tmpfs` path when the credential is session-scoped.
2. The *source* remains in the custodian — the file is a projection,
   re-creatable, rotatable, auditable; the custodian is what gets
   rotated.
3. The file's existence is listed in this document's inventory (§2 of
   the migration plan), so resident plaintext is always a known,
   bounded set — never an accident.

### 6.5 Documents (not tool secrets)

The ledger stays under Layer 1: it benefits from diff and history, and
its sensitivity profile fits the accepted trade-offs. The password
spreadsheet does not belong in this repository at all — it belongs in
a password manager, whose container is itself encrypted at rest
(memory-hard KDF, optional keyfile) and whose compromise model is
per-entry, not per-repository. *(Decision pending — Open item.)*

---

## 7. End-to-end behavior

### New machine (bootstrap)

```
clone repository
  → bootstrap unit: LoadCredentialEncrypted= → systemd-creds decrypts
    TPM-sealed passwords into $CREDENTIALS_DIRECTORY
  → exported as WITS_TRANSCRYPT_*_PASSWORD (existing env channel,
    unchanged downstream)
  → smudge + dotdrop deploy run exactly as today
```

The bootstrap path never needs the interactive agent.

### Daily use

| Action | What happens |
|--------|--------------|
| `git add` a filtered file | filter parses packet → `DERIVE` over agent socket → AEAD seal. Password never in the filter process. |
| `git clone` (fetch/push) | ssh → FIDO2 touch or session agent; no key material on this disk to steal |
| start pi / claude | config's `!command` asks the agent; key enters that tool's memory for the session |
| `git diff` a filtered file | textconv → same resolver → same agent |
| edit configs in the working tree | plaintext, as always — local disk, in use |
| log out | agent state dies with `$XDG_RUNTIME_DIR`; nothing persists |

### Loss or compromise of a machine

The disk contains: TPM-sealed blobs (unopenable off the machine),
FIDO2 handles (unopenable without the token), contained plaintext files
(§6.4 — the bounded residual list). Repository history is unaffected
(ciphertext everywhere). Response: rotate the affected context password
through the custodian, re-encrypt, and — because of A5 — treat
everything encrypted under the old password as exposed from the day it
was first pushed.

---

## 8. Security delta

| Adversary | Before | After |
|-----------|--------|-------|
| A1 forge reader | ciphertext only | unchanged |
| A2 offline cracker | unchanged (entropy-gated) | unchanged |
| A3 same-UID process | password readable from config file at any time | must target the agent socket or in-use memory; no disk source |
| A4 disk theft | passwords + SSH keys + tokens, all plaintext | sealed blobs, key handles, nothing plaintext |
| A5 future leak | full history exposed; keys scattered across files | same history exposure (irreducible), but one custodian to rotate, one inventory to audit |

The irreducible row is A5: no design can retroactively protect history
already published in ciphertext. Every other row moves.

---

## 9. Migration plan

Each step is deployable alone; each is reversible by restoring the
previous configuration.

1. **Hygiene (no dependencies).** `config.d/khronos3d_transcrypt.conf`
   to `0600`, matching `personal_transcrypt.conf`. (Superseded by step
   3 but worth doing now.)
2. **wits: split KDF from AEAD.** Crypto-path refactor so
   `encrypt/decrypt` accept an externally derived key; the Resolver
   gains the `agent` source; `status` reports it. Backward compatible —
   git-config source still works.
3. **wits agent.** Socket, `DERIVE`/`PASSWORD`/`GET`/`STATUS`/`LOCK`,
   passphrase unlock first. Filters switch to `DERIVE`.
4. **Move the passwords.** Seal into TPM (or keyring) → verify with
   `wits transcrypt status` → delete the `password` lines from both
   `config.d` files. This is the moment A4 stops applying to them.
5. **Bootstrap units.** `LoadCredentialEncrypted=` wiring; the env
   channel is fed from sealed storage instead of wherever it is fed
   from today.
6. **SSH.** `IdentityAgent` in the ssh template; then either FIDO2
   re-enrollment (preferred, needs hardware decision) or
   passphrase-protected keys.
7. **Tool credentials.** pi `!command` (verified); git credential
   helper; claude/codex after per-tool verification. Manifests drop key
   material for this class.
8. **Runbooks.** Rotation, new-machine enrollment, revoke-a-machine —
   short, written down, exercised once.

---

## 10. Decision record

**Decision.** Keep wits transcrypt as Layer 1 unchanged; add a single
custody service (session agent with TPM/keyring/passphrase unlock
sources) as Layer 2; integrate every consuming tool through its native
interface shape — oracle (ssh/FIDO2), string (`!command`, credential
helpers), contained file as last resort — as Layer 3.

**Rationale.** Layer 1 is already the strongest transparent-filter
design available and replacing it would be a downgrade. The actual
weaknesses are all operational: passwords asleep in git config, SSH
keys asleep in `~/.ssh`, tokens asleep in tool configs. One custodian
with one socket interface minimizes what any consumer must know
(SIMPLICITY: the backend diversity is absorbed inside the agent), and
the interface-shape taxonomy converts an open-ended "how does each tool
use keys?" into a bounded per-tool decision.

**Rejected.**

- *git-crypt* — downgrade on every axis: no memory-hard KDF, per-repo
  single key, no path binding, minimal maintenance.
- *git-remote-gcrypt* — encrypts the whole remote, which conflicts with
  the public dotfiles purpose and fits poorly with the forges in use.
- *age-based transparent filter* — the filter idempotence requirement
  forces deterministic encryption; off-the-shelf age tooling does not
  do deterministic streaming, so this path either fights git or becomes
  a bespoke construction — which is what wits already is, in stronger
  form.
- *Whole-repository split into a private repo* — still the right home
  for the password vault (§6.5), but as a total answer it sacrifices
  the one-clone UX and does nothing for machine-side at-rest secrets,
  which this design addresses directly.

**Complexity.** The boundary sits at the agent socket: outside it,
consumers know one verb set; inside it live unlock sources, backend
selection, KDF memoization, and sealing — essential complexity,
contained. The DERIVE endpoint removes the password from an entire
population of processes (every filter invocation) rather than adding a
flag to each. The resolver keeps its existing precedence semantics and
gains one source; no caller checks anything new.

**Risks.** Agent availability becomes a dependency of normal git
operation on interactive machines (bootstrap is immune via env);
mitigation: agent auto-start as a user unit, and the resolver's loud
failure names the fix. TPM binding makes recovery machine-local;
mitigation: the passphrase source always exists, and re-sealing is part
of the new-machine runbook. The socket is a same-UID attack surface, as
every agent before it; accepted and stated (2.3).

**Context to pin** (turn into comments at implementation):

- Why the git-config password source is removed, not deprecated
  quietly: plaintext at rest is the defect this architecture exists to
  close; a "compatible" fallback would preserve the defect.
- Why `DERIVE` exists: to keep the password inside one process
  population (the agent) instead of every filter invocation.
- Why the resolver does not know about TPM or keyring: one custody
  boundary; unlock-source diversity is internal to the agent.
- Why determinism (SIV) is accepted: filter idempotence; see the
  crypto module comment.

**Open items.** See §11.

---

## 11. Open items

1. **FIDO2 hardware.** Is a token available (two for enrollment)? If
   not, SSH falls back to passphrase + `IdentityAgent`.
2. **claude / codex native support.** Verify each tool's helper
   mechanism before wiring; wrappers otherwise.
3. **Password vault migration.** Move the password spreadsheet to a
   password manager (keepassxc / gopass). Decision pending; this design
   assumes yes.
4. **TPM PCR policy.** Seal against default PCRs or bind to boot
   state? Default: no PCR binding (firmware updates should not break
   unlock); revisit if machine-local tamper resistance matters.
5. **git credential helper.** Native `git-credential-libsecret` versus
   a `wits` helper — unification versus maintained-externally.
6. **Ledger.** Confirm it stays under Layer 1 (recommended).

---

## Appendix A — alternatives considered

**git-crypt.** Transparent filtering via GPG-unlocked symmetric key.
Rejected: unauthenticated-mode-era construction, no memory-hard KDF,
per-repository single key, no path binding, sparse maintenance. Strictly
dominated by the existing wits layer.

**git-remote-gcrypt.** Encrypts everything on the remote. Rejected:
makes the repository unmonitorable and private-everything, conflicting
with the public dotfiles role; poor fit for the smart-HTTP forges in
use; all-or-nothing (cannot share public parts of the same repo).

**Switch to a private repository for all secrets.** Removes the public
ciphertext pool entirely but also the public transport for everything
else, splits the one-clone deployment model, and leaves machine-side
at-rest exposure untouched. Adopted only in the narrow form of §6.5
(vault content leaves for a password manager).

**age / sops.** Excellent for whole-file or structured-value encryption
with public-key recipients; neither provides a deterministic streaming
filter, which the clean/smudge contract requires. `sops` remains a
reasonable tool for future structured config outside this repository's
filter paths.

**Hardware tokens everywhere (FIDO2 for every secret).** The oracle
shape does not generalize to bearer-string secrets; API tokens have no
"sign this" protocol. Adopted where the shape exists (SSH), injected
custody where it does not.
