.. _wits-transcrypt:

``wits transcrypt``
===================

Transparent file encryption, wired into git's filter system. Once set up,
committing a marked file stores ciphertext and checking it out restores
plaintext — there is nothing to remember in the day-to-day ``git add`` /
``git checkout`` workflow. It is the tool you reach for when a repository needs
to hold secrets — keys, ``.env`` files, credentials — but you do not want those
secrets sitting in history in the clear.

As a filter it is deliberately *transparent*: the files you see on disk, edit,
and diff are plaintext. Only what lands in the object store is enciphered. A
clone without the password gets the ciphertext as files — readable, useless,
but never a broken checkout.

How it hangs together
---------------------

Git supports, per path, a *clean* filter (applied to content going into the
index) and a *smudge* filter (applied to content coming out to the working
tree). ``wits transcrypt`` is both ends, plus a third for ``git diff``:

.. list-table::
   :header-rows: 1
   :widths: 32 68

   * - Command
     - What it does
   * - ``wits transcrypt clean <file>``
     - plaintext on stdin → base64 ciphertext on stdout
   * - ``wits transcrypt smudge <file>``
     - base64 ciphertext on stdin → plaintext on stdout
   * - ``wits transcrypt textconv <file>``
     - passes through the file ``git diff`` hands it — git has already
       smudged each blob into it — so diffs read as plaintext instead of two
       blobs of base64

You almost never run these by hand. You register them once, tell git which
paths they apply to, and from then on ``git add`` encrypts and ``git checkout``
decrypts on your behalf.

Getting started
---------------

This walks through encrypting a ``secrets/`` directory in an existing
repository, using the default context. You need the ``wits`` binary on your
``$PATH``.

**1. Set a password and a context salt.** These are the two things
``transcrypt`` cannot seal without. Put them in the repo's git config:

.. code-block:: sh

   git config wits.transcrypt.password 'correct horse battery staple'
   git config wits.transcrypt.salt "$(openssl rand -hex 8)"

This lives in ``.git/config``, which is local to your clone and never
committed — so the password is *not* pushed, but it does sit in plaintext on
your disk. If you would rather not store it at all (say, on CI), provide it
through the environment instead and skip this step:

.. code-block:: sh

   export WITS_TRANSCRYPT_PASSWORD='correct horse battery staple'
   export WITS_TRANSCRYPT_SALT=0123456789abcdef

The salt is not a secret, but it is part of every key: each clone that seals
needs the same one, and it changes whenever the password does (see `Salt
modes`_). Checking out needs only the password.

**2. Register the filters.** These four settings tell git what to run. Put them
in the repo's ``.git/config``, or in your global ``~/.gitconfig`` if you want
every repo to share them:

.. code-block:: sh

   git config filter.transcrypt.clean    'wits transcrypt clean %f'
   git config filter.transcrypt.smudge   'wits transcrypt smudge %f'
   git config filter.transcrypt.required true
   git config diff.transcrypt.textconv   'wits transcrypt textconv'

``%f`` is git's placeholder for the file's path — passing it matters, because
the path is bound into the ciphertext (see `The path matters`_).
``required = true`` means that if the filter ever fails, the git operation
fails loudly rather than silently committing plaintext.

**3. Mark which files to encrypt.** ``.gitattributes`` maps paths to the
filter. This file *is* committed, so everyone who clones the repo inherits the
rule:

.. code-block:: sh

   echo 'secrets/** filter=transcrypt diff=transcrypt' >> .gitattributes
   git add .gitattributes && git commit -m 'Encrypt secrets/'

**4. Add a secret.**

.. code-block:: sh

   mkdir -p secrets
   echo 'API_KEY=hunter2' > secrets/api.env
   git add secrets/api.env && git commit -m 'Add api key'

That's it. The working-tree file is plaintext; what got committed is
ciphertext.

Checking it actually worked
---------------------------

Three quick angles, from three different vantage points:

.. code-block:: sh

   # what git stored (the committed blob) — should be base64 ciphertext
   git cat-file -p HEAD:secrets/api.env

   # your working tree — plaintext
   cat secrets/api.env

   # the resolved configuration, and where each value was read from
   wits transcrypt status

``wits transcrypt status`` is also the first thing to run when something looks
off — it shows each setting and whether it came from the environment, git
config, or a built-in default. (The password itself is masked.)

Cloning the repo elsewhere
--------------------------

``.gitattributes`` travels with the repo, but the password does not. So a fresh
clone checks the secrets out as the raw encrypted base64 — readable as files,
useless as secrets, but crucially *not* a broken checkout. To turn them back
into plaintext, set the password and re-run the smudge filter:

.. code-block:: sh

   git clone <url> && cd repo
   git config wits.transcrypt.password 'correct horse battery staple'
   git config wits.transcrypt.salt 0123456789abcdef   # the repo's salt

   # force the working tree to be re-smudged now that the password exists
   rm -rf secrets && git checkout -- secrets/

The ``rm`` is there because git will not re-run the filter on files it thinks
are already up to date; removing them first makes the checkout repopulate them
through ``smudge``. This is the whole onboarding story — tell the teammate the
password and the salt, they run those lines, done.

A file that is added again while it still holds its ciphertext is stored
unchanged: clean recognises a packet that opens at that path and passes it
through, rather than sealing a packet inside a packet. A packet that does not
open there — another password's, or another path's — is refused.

Choosing algorithms
-------------------

The defaults — AES-256-GCM, PBKDF2, SHA-256 — are fine for most uses. To change
them, set the values *before* encrypting anything and keep them consistent. A
file sealed under one cipher, KDF, digest, or set of KDF parameters can only be
opened with the same ones — packets do not record them — so once files are
sealed, change these only together with the password (see `Rotating the
password`_):

.. code-block:: sh

   git config wits.transcrypt.cipher chacha20-poly1305
   git config wits.transcrypt.kdf    argon2id

Accepted values:

.. list-table::
   :header-rows: 1
   :widths: 20 30 50

   * - Setting
     - Accepted values
     - Notes
   * - ``cipher``
     - ``aes-256-gcm`` \| ``chacha20-poly1305``
     - both take a 256-bit key
   * - ``kdf``
     - ``pbkdf2`` \| ``argon2id``
     - argon2id defaults to 4 time-cost iterations over 128 MiB and 2 lanes
   * - ``digest``
     - ``sha256`` \| ``sha384`` \| ``sha512`` \| ``sha3256`` \| ``sha3384`` \|
       ``sha3512`` \| ``blake2b`` \| ``blake2s``
     - the PBKDF2 PRF; argon2id ignores it. BLAKE2 cannot be paired with
       PBKDF2 — use it with ``argon2id``, or pick a SHA digest
   * - ``memory``
     - a whole number of MiB
     - argon2id memory, default 128
   * - ``lanes``
     - a whole number
     - argon2id lanes, default 2

What a file was sealed with is the algorithm, not its spelling, so the parser
in ``wits`` is lenient about a couple of obvious renderings (``sha3-256`` for
``sha3256`` and so on).

Salt modes
----------

Nothing in a packet is computed from the password without the KDF: the IV is a
keyed hash of the path and the content, under a key the KDF derives from the
password and the *context salt*. So a published packet never lets a guess at
the password be checked for less than the KDF costs, however guessable the
file's content is. What a salt mode decides is where the packet's own salt —
and so the key its content is sealed under — comes from:

* ``file`` (the default) — a fresh random salt for every content version, so
  every version is sealed under its own key. Re-cleaning an unchanged file would
  therefore produce a new packet each time; instead, clean reads the packet the
  index already holds at that path and hands it back whenever it carries this
  content's IV, so git sees no change. Sealing a changed file costs one KDF more
  than ``context`` mode.
* ``context`` — the context salt itself, so the context has one key and sealing
  is deterministic on its own.

.. code-block:: sh

   git config wits.transcrypt.salt-mode context

Switching mode re-seals nothing that is unchanged: both modes compute the same
IV, and clean keeps any index packet that already carries it.

Multiple contexts
-----------------

A context is an independent secret set within one repository — useful when,
say, ``secrets/`` and ``prod-secrets/`` should be locked under different
passwords. Each context gets its own password, its own filter, and its own
``.gitattributes`` rule. The filter command carries the context through ``-C``:

.. code-block:: sh

   git config wits.transcrypt.prod.password 'a different password'
   git config wits.transcrypt.prod.salt "$(openssl rand -hex 8)"

   git config filter.transcrypt-prod.clean    'wits transcrypt -C prod clean %f'
   git config filter.transcrypt-prod.smudge   'wits transcrypt -C prod smudge %f'
   git config filter.transcrypt-prod.required true
   git config diff.transcrypt-prod.textconv   'wits transcrypt -C prod textconv'

   echo 'prod-secrets/** filter=transcrypt-prod diff=transcrypt-prod' >> .gitattributes

   wits transcrypt -C prod status

The context name is yours to choose; ``transcrypt-prod`` is just the *git
filter's* name and only has to match between the ``filter.*`` config and
``.gitattributes``.

Configuration reference
-----------------------

Every setting can come from the environment or from git config. For a context
named ``<ctx>``, insert it as shown; the default context omits that segment.

.. list-table::
   :header-rows: 1
   :widths: 20 34 30 16

   * - Setting
     - Environment variable
     - Git config key
     - Default
   * - Password
     - ``WITS_TRANSCRYPT_[<CTX>_]PASSWORD``
     - ``wits.transcrypt.[<ctx>.]password``
     - *required*
   * - Cipher
     - ``WITS_TRANSCRYPT_[<CTX>_]CIPHER``
     - ``wits.transcrypt.[<ctx>.]cipher``
     - ``aes-256-gcm``
   * - Digest
     - ``WITS_TRANSCRYPT_[<CTX>_]DIGEST``
     - ``wits.transcrypt.[<ctx>.]digest``
     - ``sha256``
   * - KDF
     - ``WITS_TRANSCRYPT_[<CTX>_]KDF``
     - ``wits.transcrypt.[<ctx>.]kdf``
     - ``pbkdf2``
   * - Iterations
     - ``WITS_TRANSCRYPT_[<CTX>_]ITERATIONS``
     - ``wits.transcrypt.[<ctx>.]iterations``
     - the KDF's historical default
   * - Memory (MiB)
     - ``WITS_TRANSCRYPT_[<CTX>_]MEMORY``
     - ``wits.transcrypt.[<ctx>.]memory``
     - ``128``
   * - Lanes
     - ``WITS_TRANSCRYPT_[<CTX>_]LANES``
     - ``wits.transcrypt.[<ctx>.]lanes``
     - ``2``
   * - Context salt
     - ``WITS_TRANSCRYPT_[<CTX>_]SALT``
     - ``wits.transcrypt.[<ctx>.]salt``
     - *required to seal* — 16 hex digits
   * - Salt mode
     - ``WITS_TRANSCRYPT_[<CTX>_]SALT_MODE``
     - ``wits.transcrypt.[<ctx>.]salt-mode``
     - ``file``

A dash in a key becomes an underscore in its environment name, since a shell
cannot name a variable with one.

Resolution order
~~~~~~~~~~~~~~~~

When the same setting is defined in more than one place, the first hit wins:

1. environment, context-specific — ``WITS_TRANSCRYPT_<CTX>_<KEY>``
2. environment — ``WITS_TRANSCRYPT_<KEY>``
3. git config, context-specific — ``wits.transcrypt.<ctx>.<key>``
4. git config — ``wits.transcrypt.<key>``
5. built-in default

Environment beats git config because it is the more deliberate, throwaway
override. Steps 2 and 4 — the context-less fallbacks — are **only** taken for
the ``default`` context: a non-default context never borrows the bare key, so a
``prod`` operation cannot silently pick up the ``default`` password and seal
data under the wrong one.

There is no context-less aliasing trick: ``WITS_TRANSCRYPT_PASSWORD`` and
``wits.transcrypt.password`` mean exactly the ``default`` context, and nothing
else is reached from another context.

The path matters
~~~~~~~~~~~~~~~~

The clean/smudge filters use the file's path (the ``%f`` you passed) as part
of the encryption, binding each ciphertext to its location. Moving an encrypted
blob to a different path makes it fail to authenticate rather than silently
decrypt — which is why ``%f`` belongs in the filter command, and why the same
path must be intact when decrypting. Re-cleaning unchanged content gives git
the blob it already holds (that is the determinism guarantee, see `Salt
modes`_), and the path is part of what makes that stable and safe.

Rotating the password
---------------------

The password, the context salt, and the KDF parameters change together:
packets record none of them, so a new parameter on its own would leave every
existing file unopenable, and a salt kept across a rotation would let work
precomputed against the old one carry over. Seal everything again under the
new values:

.. code-block:: sh

   git config wits.transcrypt.password 'the new password'
   git config wits.transcrypt.salt "$(openssl rand -hex 8)"
   git add --renormalize -- secrets/ && git commit -m 'Rotate the secrets password'

Every other clone then needs the new password and salt before it checks out the
rotated files, and history sealed under the old password still needs the old
one — supply it, with the old parameters, through the environment for the
command that reads that history.

Missing vs. wrong password
--------------------------

These two cases are treated very differently on purpose.

* **No password configured.** ``smudge`` passes the encrypted bytes through
  untouched. A teammate who clones without the key gets unreadable files and a
  *working* checkout, rather than a wedged repository.
* **Wrong password.** This fails loudly: a packet the configured password does
  not open means the key is wrong, and a checkout that quietly kept the
  ciphertext would hide that. If you need to force the raw ciphertext through
  anyway — say, to unblock a checkout and investigate — set
  ``WITS_TRANSCRYPT_ALLOW_RAW_FALLBACK=1``.

A third case is content that is not a transcrypt packet at all: a file matched
by ``.gitattributes`` but committed before encryption was set up, or a binary
blob. Decryption is never attempted on these — they are passed through
untouched — so ``git diff`` and checkout keep working instead of aborting. Only
a genuine packet the configured settings do not open trips the loud failure
above — and so does a setting that is present but malformed, such as an
unknown cipher, which is never mistaken for a missing password.

Troubleshooting
---------------

.. list-table::
   :header-rows: 1
   :widths: 42 58

   * - Symptom
     - Cause and fix
   * - ``git add`` of a secret aborts with *no password configured*
     - Set ``wits.transcrypt.password`` (or ``WITS_TRANSCRYPT_PASSWORD``)
       before adding. With ``required = true``, a failing filter correctly
       stops the operation.
   * - ``git add`` of a secret aborts with *no context salt configured*
     - Set ``wits.transcrypt.salt`` (or ``WITS_TRANSCRYPT_SALT``) to 16 hex
       digits — ``openssl rand -hex 8`` makes one — and use the same salt on
       every clone that seals.
   * - ``git add`` aborts with *refusing to seal a packet inside a packet*
     - The working-tree file holds ciphertext that the configured settings do
       not open at that path — copied from elsewhere, or sealed under another
       password. Put the plaintext back, or fix the password and re-checkout
       the path.
   * - Working-tree files are base64 gibberish
     - The password is not set in this clone, so ``smudge`` passed the
       ciphertext through. Set the password, then ``rm`` and re-checkout the
       paths.
   * - Checkout fails with an authentication error
     - Wrong password (or the file moved to a path that does not match how it
       was encrypted). Fix the password, or set
       ``WITS_TRANSCRYPT_ALLOW_RAW_FALLBACK=1`` to check out the raw bytes and
       investigate.
   * - Old files will not decrypt after changing ``cipher``, ``kdf``,
       ``digest``, ``iterations``, ``memory``, or ``lanes``
     - Those settings are part of how each file was sealed, and packets do not
       record them. Restore the previous values, or decrypt with the old
       settings and re-encrypt with the new ones (see `Rotating the
       password`_).
   * - ``wits transcrypt status`` shows a value you did not expect
     - Check the source column — an environment variable will quietly override
       git config. The resolution order above explains who wins.
