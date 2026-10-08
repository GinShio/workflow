---
name: handoff
description: Compact the current conversation into a handoff document for another agent to pick up.
argument-hint: "What will the next session be used for?"
disable-model-invocation: true
---

Write a handoff document summarising the current conversation so a fresh agent can continue the work. Save it as `<topic>.md` in this directory, not the current workspace, and tell the user its path:

```sh
common=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) && key=${common%/.git} || key=$PWD
dir=${WITS_LLM_HANDOFF_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/wits/handoff}$key
mkdir -p "$dir"
```

Include a "suggested skills" section in the document, naming which skills the next agent should load.

Do not duplicate content already captured in other artifacts (specs, plans, ADRs, issues, commits, diffs). Reference them by path or URL instead.

Separate what you verified from what you only assumed: the next agent treats the document as a contract and will not re-check it.

Redact any sensitive information, such as API keys, passwords, or personally identifiable information.

If the user passed arguments, treat them as a description of what the next session will focus on and tailor the doc accordingly.
