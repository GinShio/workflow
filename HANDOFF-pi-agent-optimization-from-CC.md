# Handoff: pi prompts/skills optimization baseline (from CC, 2026-09-10)

## Purpose & authority

This is the working baseline for the next phase: deep optimization of the pi
agent's skills / prompts / extensions. It records four decisions approved in
discussion on 2026-09-10, the adjudicated facts they rest on, and the open
questions each implementation must settle. **Nothing has been implemented yet**
— every item below needs its design-before-implement pass per the working
agreement.

Evidence base: a full analysis of all 51 pi sessions (2026-09-04 → 09-09) plus
a complete source read of the deployed config. Companion doc with the
quantitative and qualitative detail: `HANDOFF-pi-usage-analysis.md` (same
directory; status — written by CC earlier the same day, initially unrequested,
now serving as the evidence annex; keep-or-delete is the user's call).

Usage timeline for orientation (user's own summary): configure pi agent → yavk
implementation → yavk review → pi agent enhancement + pi-discipline + hooks →
token/context handoffs.

## Adjudicated facts (load-bearing — do not re-derive)

These were explicitly verified this session; several corrected earlier beliefs.

1. **pi-discipline provides the `todo` and `protocol` tools.** pi 0.84.4 core
   ships no native todo/protocol (tool constructors are Bash/Coding/Edit/Find/
   Grep/Ls/PowerShell/ReadOnly/Read/Write only); session toolResults carry
   pi-discipline's exact data shapes (`criterion` field; `ProtocolDetails`
   with skill path); repo source == deployed file. Usage: todo 158 calls in 3
   days (81/47/30 by day), protocol 12 (6/6/0, declining). The *concepts*
   came from the CC-vs-PI reviews; the *mechanism* is pi's extension API; the
   implementation and registration are pi-discipline's.
2. **promptSnippet mechanics** (pi docs, `extensions.md`): the default system
   prompt has an "Available tools" section; `promptSnippet` opts a custom tool
   into a one-line entry there, `promptGuidelines` appends to "Guidelines".
   **Without promptSnippet a custom tool is absent from that section** — the
   schema still goes over the API tools parameter, but the always-visible text
   is the reliable attention anchor. Neither field is inherited by tool
   overrides; overrides must re-declare. There is no central registry — the
   metadata is declared per tool at `registerTool` time.
3. **Compaction never fired and never will under current usage.** GLM-5.3-flash
   has a 1M context window (models-store.json; maxTokens 131072); observed peak
   295k over 6 days, zero compaction/branch_summary entries anywhere. The user
   additionally **deliberately restarts sessions to keep context small** —
   attention-driven intelligence degrades with context size, so they avoid
   filling the window by design (stated 2026-09-10). Therefore pi-discipline's
   post-compaction re-injection is permanently moot in practice.
4. **pi-subagents suppresses AGENTS.md/CLAUDE.md in subagent system prompts**
   (`agent-runner.js:499`, prompt_mode: replace + isolated). Consequences:
   `agents/puppet.md`'s identity block is load-bearing, not duplication; and
   any future *implementing* subagent must be handed the working agreement
   explicitly.
5. **Environment decisions by the user**: web_search is fully disabled already
   (removed alongside the workflow tooling); `~/.local/state/pi` stays outside
   the read allowlist deliberately (session-history reading is a rare task,
   not worth a permanent rule). Both earlier CC suggestions withdrawn.
6. **Baseline economics**: idle baseline ~7.2k tokens (halved from 14k by the
   user's own pruning); AGENTS.md ≈ 1.8k of it; cacheRead:input ≈ 178M:10.7M
   (94% hit). Token savings are secondary to attention-weight savings — the
   argument for compression is relative weight of what remains.
7. **CC-vs-PI diagnosis** (verified against both session logs; the driver of
   items 1–2): the gap was mechanical, not model knowledge — protocol delivery
   (CC force-injects, pi relied on the model reading skill files once), no task
   scaffolding ("tail collapse": one 9-minute write burst, one write per file,
   never revisited; CC: 117 edits vs 56 writes), no self-revision rhythm, zero
   FFI coverage in craft/. Adopted so far: verification criteria in
   implementation-protocol, pi-discipline todo+protocol, promptSnippet (which
   fixed custom tools being invisible in the system prompt). Still missing:
   craft/FFI.md, a verify-gate mechanism ("tests green" masked the worst
   defects in both tools), and any A/B retest.

Standing user constraints (unchanged): no memory features, no mega-collections
(oh-my-pi / gsd / superpowers class), no MCP.

## Item 1 — AGENTS.md: radical compression into map form (approved)

Current: 6.6KB / ~1.8k tokens, sections: Precedence, About me, HITL (stance,
why-it-exists, three gates, Act/Ask/Never decision map), Quality-bar pointers.

Rationale established this session: the mechanical layer (hooks/rules.json +
pi-guard) has absorbed most of the action-level enumerations — git side
effects, package/dependency changes, writes outside the workspace, destructive
commands, secret *paths*. Hooks gate surfaces (command text, paths); they
cannot gate semantics. The text layer should own only what no hook can express,
plus the map.

Target sketch (user's own draft, 2026-09-10 — quoted structure verbatim):

```
1. Who       domain weights (compilers/GPU/Vulkan) + language conventions
2. Priority  Never/Always marker semantics in 2 sentences
             + "action-level rules: single source of truth is the hooks layer"
3. Unhooked  the Nevers a guard cannot express: invented APIs, echoing secret
             CONTENT, test skip semantics, no recipe → don't guess builds
4. Map       craft/ one line, skills one line, puppet/research one line
```

Implementation notes:

- The user's Unhooked list deliberately **drops** two entries from the current
  Never list: "loosen a rule inherited from a higher layer" and "route around
  a gate by delegating". Recorded as intentional.
- The three-gate loop (Understand/Design/Implement) and the golden rule are
  not in the sketch. Open question: compress them into the Priority block, or
  delegate them to the protocol skills entirely (AGENTS.md already says "the
  protocols run these same gates, under these same names"). Behavioral
  evidence says the gate *structure* shaped real behavior (design.md before
  implementation; handover Authority/Not-verified fields) — compress wording,
  do not silently lose the structure. This is a design decision for the
  implementation session.
- Expected size: ~6.6KB → ~3KB. The win is attention weight, not tokens.
- **Verification is mandatory** (the user's own standard: a no-op call is
  model-relative and settles by running): same-task A/B batch, old vs new
  AGENTS.md, comparing ask-rate, overreach rate, and output quality, before
  replacing the deployed file.

## Item 2 — pi-discipline slim-down / split (approved)

Target shape: keep `todo` + `protocol` tools with their
`promptSnippet`/`promptGuidelines`; **delete the compaction re-injection
machinery** (~40% of the 534-line source: the `session_compact` handler,
`windowStartIndex`, `lastRenderIndex`, `ReinjectionDetails`, the
`custom_message` render bookkeeping). The code already documents the degrade
path ("recoverable via todo list and protocol") — deleting re-injection just
makes that path the only path.

- Split form (one slim extension vs. a tools-extension + prompt-metadata
  sub-extension) is an open design decision; the user sketched "todo +
  protocol + promptSnippet/promptGuidelines as a sub-extension".
- Rely on the user's context philosophy (fact 3): sessions end before windows
  fill; compaction-replacement packages (blackhole etc. from
  `HANDOFF-pi-context-optimization-research.md`) are out of scope unless that
  philosophy changes.
- Fix while in there: todo #6 criterion-update render defect (observed in the
  09-07 session; update calls render stale plan text).
- promptSnippet deep-dive deferred as its own future discussion (user asked
  for it explicitly): measure whether snippet presence moves call rates
  (especially for npm-package tools), and the override path for re-declaring
  metadata on tools pi-discipline does not own.

## Item 3 — adopt handoff + grill-with-docs (approved), wayfinder protocol absorbed

From [mattpocock/skills](https://github.com/mattpocock/skills). The user
already runs its `writing-for-agents` skill; the three picks form a coherent
system aligned with the context philosophy — **handoff = the session-boundary
primitive (the manual replacement for compaction), grill-with-docs =
single-session planning, wayfinder = multi-session map**.

- **handoff** — adopt nearly as-is. Formalizes the user's existing HANDOFF
  convention (two pi handoffs already exist in this repo) and adds the
  missing piece: explicit when-to-write / when-not-to-write rules (CC
  violated exactly this on 2026-09-09 by writing a handoff unasked).
- **grill-with-docs** — stateful grilling: interview until every branch of
  the design tree resolves, persisting learnings into project docs. Adaptation
  point: it writes CONTEXT.md + ADRs; this user's equivalent is per-project
  design docs (e.g. yavk's `docs/bootstrap-design.rst`, plain-text readable
  rst per the docs standard). The user's decision: **grilling +
  domain-modeling load as progressive disclosure** under grill-with-docs, not
  as always-present separate skills.
- **wayfinder** — do not install now. Its operating model (one decision ticket
  per session, handoff in/out, map as plain-text index with Destination /
  Decisions so far / fog-of-war / Out of scope) matches the user's method, but
  it is built on an issue tracker (labels, child issues, self-assign) and
  schedules the whole matt family. Decision: absorb the *protocol* (map file
  structure, ticket types grilling/prototype/research/task, frontier concept),
  implement later only when a genuinely multi-session foggy effort appears
  (candidate: yavk's next layer). puppet already covers its research-ticket
  type. matt's own changeset flags over-reaching for wayfinder as the known
  misuse.
- All adoptions follow the standing meta-rule: rewrite into house style per
  `writing-for-agents` (own leading words, no foreign vocabulary), single
  skills, no bundle.
- **Blocked on source texts**: CC's environment cannot fetch github.com /
  raw.githubusercontent.com / skillvault.md (network policy). The user pastes
  the three SKILL.md files, or re-tries from a network with access.

## Item 4 — thinking effort (deferred; problem recorded for that discussion)

Deferred by the user to a dedicated future discussion. The record for it:

- All 13 provider error stops in the flagship session (yavk, 09-05) share one
  signature: usage≈0 with 13–42k chars of thinking already emitted — the
  provider dies mid-ultra-long-thinking. Each retry burned ~5min; a
  five-failure streak cost 26 minutes. Same session, same thinking=max, once
  responses got shorter: zero failures.
- thinking:text ratio ≈ 15:1 (5.27M : 0.36M chars across all sessions);
  reasoning is 1.1M of 1.7M output tokens.
- The tradeoff as the user framed it: max protects intelligence; max × long
  single responses is the failure mode; lowering the level feels like
  sacrificing intelligence — "十分痛苦".
- Levers on the table (to be argued in that discussion, none decided):
  1. Response-length discipline front-loaded in prompts ("small, verified
     increments") — attacks the actual failure variable while keeping max.
  2. Escalation policy instead of a fixed level: default high, max at design
     gates and when the 2-3-failure rule trips. pi supports mid-session
     thinking_level_change; protocol gate transitions are natural escalation
     points.
  3. Auto-continue extension: inject a continue-steer after repeated
     stopReason=error instead of waiting for the user's "請繼續".
  4. Retry tuning (current: pi-level 3 retries @2s base; provider-level 0).
  5. The unknown that should settle it: high-vs-max intelligence delta on
     this model/task mix — A/B batch, same as items 1–3's method.

## Identified but not scheduled (from the same discussion)

- **design-protocol**: two Output templates ≈ 40% of the file and partially
  duplicate the "Document" step — compress; merge "After convergence" into
  "Stance".
- **implementation-protocol**: "Record the plan in the harness's task tool
  where one exists" → name the `todo` tool explicitly (the one weak wiring
  point; todo itself is heavily used).
- **prompts/research.md**: zero invocations in 6 days — prune, or repurpose
  when /research-style flows consolidate.
- **craft/FFI.md** + a **verify-gate** mechanism: adopted-but-unbuilt items
  from the CC-vs-PI plan; the three worst defects in both tools' outputs were
  FFI-layer bugs that "tests green" did not catch.
- **npm-package tool visibility**: whether rpiv/subagents tools declare
  promptSnippet is unverified; check when the promptSnippet discussion happens.

## Open questions (each blocks its item's implementation)

1. Item 1: where do the golden rule + three gates live in map form —
   compressed into Priority, or delegated to the protocol skills?
2. Item 1: the A/B verification protocol (task batch selection, metrics).
3. Item 2: one slim extension or tools + prompt-metadata split? naming?
4. Item 3: paste source texts (blocked fetch); grill-with-docs doc-target
   convention per project.
5. Item 4: the dedicated thinking-effort discussion itself.

## File map (deployed)

- `~/.config/pi/agent/`: settings.json, AGENTS.md, extensions/{pi-discipline,
  pi-guard}, hooks/{rules.json, core/mod.ts}, agents/puppet.md,
  prompts/research.md, craft/{QUALITY-BAR,SIMPLICITY,COMMENTS}.md
- `~/.agents/skills/`: design-protocol, implementation-protocol (+commit.md),
  review-protocol, wits-review (+draft.md), writing-for-agents
  (+SKILL-MECHANICS.md)
- Sessions: `~/.local/state/pi/sessions/*.jsonl` (read deliberately blocked
  for agents; schema notes in the evidence annex)
- Source of truth for all of the above: `dotfiles/agents/common/` (dotdrop)
- Analysis scripts/digests were under `/tmp/pi_analysis/` (volatile; the
  extraction recipe is described in the evidence annex and reproducible).
