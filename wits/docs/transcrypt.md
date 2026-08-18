# `wits transcrypt`

Transparent file encryption, wired into git's filter system. Once set up,
committing a marked file stores ciphertext and checking it out restores
plaintext — there is nothing to remember in the day-to-day `git add` /
`git checkout` workflow. It's the tool you reach for when a repository needs to
hold secrets (keys, `.env` files, credentials) but you don't want those secrets
sitting in the history in the clear.

## How it hangs together

Git supports, per path, a *clean* filter (applied to content going into the
index) and a *smudge* filter (applied to content coming out to the working
tree). `wits transcrypt` is both ends:

- `wits transcrypt clean <file>` — plaintext on stdin, base64 ciphertext on stdout.
- `wits transcrypt smudge <file>` — base64 ciphertext on stdin, plaintext on stdout.
- `wits transcrypt textconv <file>` — decrypts a file for `git diff`, so diffs read
  as plaintext instead of as two blobs of base64.

You almost never run these by hand. You register them once, tell git which paths
they apply to, and from then on `git add` encrypts and `git checkout` decrypts on
your behalf.

## Getting started

This walks through encrypting a `secrets/` directory in an existing repository,
using the default context. You need the `wits` binary on your `$PATH`.

### 1. Set a password

The password is the only thing `transcrypt` can't work without. Put it in the
repo's git config:

```sh
git config wits.transcrypt.password 'correct horse battery staple'
```

This lives in `.git/config`, which is local to your clone and never committed —
so the password is *not* pushed, but it does sit in plaintext on your disk. If
you'd rather not store it at all (e.g. on CI), provide it through the
environment instead and skip this step:

```sh
export WITS_TRANSCRYPT_PASSWORD='correct horse battery staple'
```

### 2. Register the filters

These four settings tell git what to run. Put them in the repo's `.git/config`,
or in your global `~/.gitconfig` if you want every repo to share them:

```sh
git config filter.transcrypt.clean    'wits transcrypt clean %f'
git config filter.transcrypt.smudge   'wits transcrypt smudge %f'
git config filter.transcrypt.required true
git config diff.transcrypt.textconv   'wits transcrypt textconv'
```

`%f` is git's placeholder for the file's path — passing it matters, because the
path is bound into the ciphertext (see [The path matters](#the-path-matters)).
`required = true` means that if the filter ever fails, the git operation fails
loudly rather than silently committing plaintext.

### 3. Mark which files to encrypt

`.gitattributes` maps paths to the filter. This file *is* committed, so everyone
who clones the repo inherits the rule:

```sh
echo 'secrets/** filter=transcrypt diff=transcrypt' >> .gitattributes
git add .gitattributes && git commit -m 'Encrypt secrets/'
```

### 4. Add a secret

```sh
mkdir -p secrets
echo 'API_KEY=hunter2' > secrets/api.env
git add secrets/api.env && git commit -m 'Add api key'
```

That's it. The working-tree file is plaintext; what got committed is ciphertext.

## Checking it actually worked

Three quick angles, from three different vantage points:

```sh
# what git stored (the committed blob) — should be base64 ciphertext
git cat-file -p HEAD:secrets/api.env

# your working tree — plaintext
cat secrets/api.env

# the resolved configuration, and where each value was read from
wits transcrypt status
```

`wits transcrypt status` is also the first thing to run when something looks off —
it shows each setting and whether it came from the environment, git config, or a
built-in default.

## Cloning the repo elsewhere (and onboarding teammates)

`.gitattributes` travels with the repo, but the password does not. So a fresh
clone checks the secrets out as the raw encrypted base64 — readable as files,
useless as secrets, but crucially *not* a broken checkout. To turn them back
into plaintext, set the password and re-run the smudge filter:

```sh
git clone <url> && cd <repo>
git config wits.transcrypt.password 'correct horse battery staple'

# force the working tree to be re-smudged now that the password exists
rm -rf secrets && git checkout -- secrets/
```

The `rm` is there because git won't re-run the filter on files it thinks are
already up to date; removing them first makes the checkout repopulate them
through `smudge`.

## Choosing algorithms

The defaults (AES-256-GCM, PBKDF2, SHA-256) are fine for most uses. To change
them, set the values *before* encrypting anything and keep them consistent — a
file encrypted under one cipher/KDF/digest can only be decrypted with the same
ones:

```sh
git config wits.transcrypt.cipher chacha20-poly1305
git config wits.transcrypt.kdf    argon2id
```

## Multiple contexts

A context is an independent secret set within one repository — useful when, say,
`secrets/` and `prod-secrets/` should be locked under different passwords. Each
context gets its own password, its own filter, and its own `.gitattributes`
rule. The filter command carries the context through `-C`:

```sh
git config wits.transcrypt.prod.password 'a different password'

git config filter.transcrypt-prod.clean    'wits transcrypt -C prod clean %f'
git config filter.transcrypt-prod.smudge   'wits transcrypt -C prod smudge %f'
git config filter.transcrypt-prod.required true
git config diff.transcrypt-prod.textconv   'wits transcrypt -C prod textconv'

echo 'prod-secrets/** filter=transcrypt-prod diff=transcrypt-prod' >> .gitattributes

wits transcrypt -C prod status
```

The context name is yours to choose; `transcrypt-prod` is just the git filter's
name and only has to match between the `filter.*` config and `.gitattributes`.

## Configuration reference

Every setting can come from the environment or from git config. For a context
named `<ctx>`, insert it as shown; the default context omits that segment.

| Setting | Environment variable | Git config key | Default |
|---|---|---|---|
| Password | `WITS_TRANSCRYPT_[<CTX>_]PASSWORD` | `wits.transcrypt.[<ctx>.]password` | *(required)* |
| Cipher | `WITS_TRANSCRYPT_[<CTX>_]CIPHER` | `wits.transcrypt.[<ctx>.]cipher` | `aes-256-gcm` |
| Digest | `WITS_TRANSCRYPT_[<CTX>_]DIGEST` | `wits.transcrypt.[<ctx>.]digest` | `sha256` |
| KDF | `WITS_TRANSCRYPT_[<CTX>_]KDF` | `wits.transcrypt.[<ctx>.]kdf` | `pbkdf2` |
| Iterations | `WITS_TRANSCRYPT_[<CTX>_]ITERATIONS` | `wits.transcrypt.[<ctx>.]iterations` | historical default for the KDF |

Accepted values: cipher `aes-256-gcm` \| `chacha20-poly1305`; KDF `pbkdf2` \|
`argon2id`; digest `sha256` \| `sha384` \| `sha512` \| `sha3256` \| `sha3384` \|
`sha3512` \| `blake2b` \| `blake2s` (note: BLAKE2 can't be paired with PBKDF2 —
use it with `argon2id`, or pick a SHA digest).

### Resolution order

When the same setting is defined in more than one place, the first hit wins:

1. Environment variable, context-specific — `WITS_TRANSCRYPT_<CTX>_<KEY>`
2. Environment variable — `WITS_TRANSCRYPT_<KEY>`
3. Git config, context-specific — `wits.transcrypt.<ctx>.<key>`
4. Git config — `wits.transcrypt.<key>`
5. Built-in default

Environment beats git config because it's the more deliberate, throwaway
override. Steps 2 and 4 — the context-less fallbacks — are **only** taken for
the `default` context: a non-default context never borrows the bare key, so a
`prod` operation can't silently pick up the `default` password and seal data
under the wrong one.

### The path matters

The clean/smudge filters use the file's path (the `%f` you passed) as part of
the encryption, binding each ciphertext to its location. Moving an encrypted
blob to a different path makes it fail to authenticate rather than silently
decrypt — which is why `%f` belongs in the filter command, and why the same path
must be intact when decrypting.

## Missing vs. wrong password

These two cases are treated very differently on purpose:

- **No password configured.** `smudge` passes the encrypted bytes through
  untouched. A teammate who clones without the key gets unreadable files and a
  *working* checkout, rather than a wedged repository.
- **Wrong password.** This fails loudly, because quietly writing corrupted
  plaintext into the working tree is worse than stopping. If you need to force
  the raw ciphertext through anyway — say, to unblock a checkout and investigate
  — set `WITS_TRANSCRYPT_ALLOW_RAW_FALLBACK=1`.

A third case is content that isn't a transcrypt packet at all: a file matched by
`.gitattributes` but committed before encryption was set up, or a binary blob.
Decryption is never attempted on these — they're passed through untouched — so
`git diff` and checkout keep working instead of aborting. Only a genuine packet
under the wrong password trips the loud failure above.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `git add` of a secret aborts with "no password configured" | Set `wits.transcrypt.password` (or `WITS_TRANSCRYPT_PASSWORD`) before adding. With `required = true`, a failing filter correctly stops the operation. |
| Working-tree files are base64 gibberish | The password isn't set in this clone, so `smudge` passed the ciphertext through. Set the password, then `rm` and re-checkout the paths. |
| Checkout fails with an authentication error | Wrong password (or the file was moved to a path that doesn't match how it was encrypted). Fix the password, or set `WITS_TRANSCRYPT_ALLOW_RAW_FALLBACK=1` to check out the raw bytes and investigate. |
| Old files won't decrypt after you changed `cipher`/`kdf`/`digest` | Those settings are part of how each file was sealed. Restore the previous values, or decrypt with the old settings and re-encrypt with the new ones. |
| `wits transcrypt status` shows a value you didn't expect | Check the source column — an environment variable will quietly override git config. The resolution order above explains who wins. |
