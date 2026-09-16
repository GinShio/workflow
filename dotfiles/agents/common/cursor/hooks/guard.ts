/**
 * Cursor adapter — one script for the gating events.
 *
 * Runs under the host node — the same runtime the agent CLIs themselves need,
 * so the guard adds no dependency of its own (native TS stripping: node ≥ 22.18).
 *
 * beforeShellExecution and beforeMCPExecution can truly escalate to the user
 * (`permission: "ask"`); beforeReadFile and preToolUse are deny-only, so their
 * ask verdicts degrade to deny+reason — never a silent allow (preToolUse's ask
 * is accepted but unenforced, which amounts to allowing). beforeMCPExecution
 * carries unstructured MCP payloads: wired but unmapped in v1 (no intent is
 * built for it, so its ASK_CAPABLE membership only matters if a mapping
 * lands). A crash exits 2 — Cursor's deny.
 *
 * No verdict resolves to an explicit `permission: "allow"`, not to silence.
 * Emitting it is Cursor's documented idiom for "proceed" (cursor.com/docs/agent/hooks
 * shows it as the normal response in every permission-hook example), and the
 * hooks.json entries carry failClosed: true, which counts an empty stdout among
 * the failures that block. Silence would therefore read as a broken guard on the
 * commonest path of all — every action no rule matches. The core still never
 * resolves to allow: this is the adapter naming "no objection" in the host's
 * vocabulary, the same translation codex makes with a silent exit 0. Cursor
 * merges sources as deny > ask > allow, so it can never overrule another hook.
 *
 * Unattended runs (AGENTS_UNATTENDED=1, exported by the launcher when nobody
 * will answer prompts): on the ask-capable events, a verdict the table marked
 * unattended.allow resolves to "allow" — the user's standing pre-approval,
 * honored only unattended — and any other ask resolves to "deny", because
 * headless asks have no one to answer them. Attended behavior is unchanged.
 * Note the coverage edge: Cursor cloud agents do not run user-level hooks
 * (~/.cursor/hooks.json) — this guard covers local sessions only.
 */

import { evaluate, isUnattended, workspaceRoot, type Intent } from "./core/mod.ts"; // paths are written against the deployed layout (~/.cursor/hooks/) — guard.ts sits beside core/, not against this repo tree
import { homedir } from "node:os";
import { readFileSync, writeSync } from "node:fs";
import process from "node:process";

const ASK_CAPABLE = new Set(["beforeShellExecution", "beforeMCPExecution"]);

const OUT = new TextEncoder();

/** Cursor's permission response; messages ride along only on a refusal. */
function respond(permission: "allow" | "ask" | "deny", message?: string) {
	const body = message
		? { permission, user_message: message, agent_message: message }
		: { permission };
	writeSync(1, JSON.stringify({ permission, user_message: message, agent_message: message }));
}

function main() {
	const raw = readFileSync(0, "utf8");
	const event = JSON.parse(raw) as {
		hook_event_name?: string;
		command?: string;
		path?: string;
		file_path?: string;
		tool_name?: string;
		tool_input?: Record<string, string>;
		cwd?: string;
	};
	const name = event.hook_event_name ?? "";
	// Events normally carry cwd; the homedir fallback keeps the scope classes
	// meaningful if one ever omits it — a "/" fallback would exempt the whole
	// filesystem from them.
	const cwd = event.cwd ?? homedir();
	const unattended = isUnattended();

	let intent: Intent | null = null;
	if (name === "beforeShellExecution") {
		intent = event.command ? { kind: "bash", command: event.command } : null;
	} else if (name === "beforeReadFile") {
		const p = event.path ?? event.file_path;
		intent = p ? { kind: "read", path: p } : null;
	} else if (name === "preToolUse") {
		// Cursor's documented preToolUse matcher list is Shell / Read / Write /
		// Grep / Delete / Task plus MCP:<name>, and the Claude compatibility
		// map sends Edit to Write — but the native in-place editor arrives as
		// StrReplace, unmapped. Write and StrReplace both carry the path in
		// tool_input.path (file_path is the Claude-shaped alias). hooks.json
		// narrows the matcher to the same three names; the test is repeated
		// here because the matcher lives in a different file and Cursor merges
		// four config sources, so this adapter cannot assume which matcher
		// (if any) selected it.
		const ti = event.tool_input ?? {};
		const tn = event.tool_name ?? "";
		const p = ti.path ?? ti.file_path;
		if ((tn === "Write" || tn === "Delete" || tn === "StrReplace") && p) {
			intent = { kind: "write", path: p };
		}
	}

	const verdict = intent && evaluate(intent, { cwd, home: homedir(), projectRoot: workspaceRoot(cwd) });
	if (!verdict) {
		await respond("allow");
		return;
	}

	// Ask-capable events can escalate to the user; the deny-only events degrade
	// every ask to deny. Unattended mode resolves asks without a human. Each
	// degradation carries the reason it happened: the verdict text alone would
	// read as an absolute rule, and a deny-only event's silence is not the same
	// fact as an unattended auto-deny — the user's next move differs.
	let permission: "allow" | "ask" | "deny";
	let suffix = "";
	if (verdict.action === "deny") {
		permission = "deny";
	} else if (!ASK_CAPABLE.has(name)) {
		permission = "deny";
		suffix = ` [${name} cannot prompt — put the request to the user and wait]`;
	} else if (!unattended) {
		permission = "ask";
	} else if (verdict.unattended === "allow") {
		permission = "allow";
		suffix = " [unattended: pre-approved by rules.json]";
	} else {
		permission = "deny";
		suffix = " [unattended: auto-denied — no user to ask]";
	}
	respond(permission, `[${verdict.rule}] ${verdict.reason}${suffix}`);
}

try {
	main();
} catch (e) {
	console.error(`guard failed: ${e}`);
	process.exit(2);
}
