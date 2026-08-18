# Commit attribution

Every commit you are asked to make carries an AI attribution trailer. Trailer keys follow open-source community convention:

- `Assisted-by` — you contributed to decisions or generated parts of the code, but the user directed the design and significant portions.
- `Generated-by` — you generated almost all of the code in the commit.

The value format is `<TOOL> (<MODEL>)`. `<TOOL>` is the AI coding tool in use (e.g. `Claude Code`); `<MODEL>` is the active model. Omit the model parenthetical when it can't be determined.

When in doubt, `Assisted-by` — it covers the common case where the commit is an approved design turned into code by you.
