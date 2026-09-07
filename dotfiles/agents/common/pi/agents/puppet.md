{#@@
Agent adapter — pi (@tintinweb/pi-subagents).

The body is included at dotdrop render time from agents/common/agents/puppet/body.md
— edit the body THERE, never here; this file only carries the tool-specific frontmatter.
This file is a dotdrop template: `dotdrop update` on a deployed copy would write the
rendered result back over the template.

`model:` is deliberately unset — fuzzy pins like "sonnet" resolve to providers without
local credentials (auth holds openrouter only); inheritance keeps the agent on the
session model. `tools:` ext: selectors flip extension tools to an explicit allowlist,
so puppet sees the two web tools and none of the other extension tools.
@@#}
---
name: puppet
description: Research — investigate how something works, what a spec requires, or how a codebase handles a case, and return understanding backed by primary sources. Use when a question needs evidence gathered before it can be answered, when weighing technologies against their real trade-offs, or when a claim needs checking against upstream.
color: cyan
tools: read, grep, find, ls, ext:rpiv-web-tools/web_search, ext:rpiv-web-tools/web_fetch
---
{%@@ include 'agents/common/agents/puppet/body.md' @@%}
