/**
 * Cursor adapter — one script for the gating events.
 *
 * beforeShellExecution and beforeMCPExecution can truly escalate to the user
 * (`permission: "ask"`); beforeReadFile and preToolUse are deny-only, so their
 * ask verdicts degrade to deny+reason — never a silent allow (preToolUse's ask
 * is accepted but unenforced, which amounts to allowing). beforeMCPExecution
 * carries unstructured MCP payloads: wired but unmapped in v1 (no intent is
 * built for it, so its ASK_CAPABLE membership only matters if a mapping
 * lands). A crash exits 2 — Cursor's deny.
 *
 * Unattended runs (AGENTS_UNATTENDED=1, exported by the launcher when nobody
 * will answer prompts): on the ask-capable events, a verdict the table marked
 * unattended.allow resolves to "allow" — the user's standing pre-approval,
 * honored only unattended — and any other ask resolves to "deny", because
 * headless asks have no one to answer them. Attended behavior is unchanged.
 * Note the coverage edge: Cursor cloud agents do not run user-level hooks
 * (~/.cursor/hooks.json) — this guard covers local sessions only.
 */

import { evaluate, workspaceRoot, type Intent } from "./core/mod.ts"; // paths are written against the deployed layout (~/.cursor/hooks/) — guard.ts sits beside core/, not against this repo tree
import { homedir } from "node:os";

const ASK_CAPABLE = new Set(["beforeShellExecution", "beforeMCPExecution"]);

async function main() {
	const raw = await new Response(Deno.stdin.readable).text();
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
	// Attendance signal shared by every adapter. Read inside main so a missing
	// --allow-env grant lands in the exit-2 crash path (failClosed blocks) —
	// not an uncaught top-level throw.
	const unattended = Deno.env.get("AGENTS_UNATTENDED") === "1";

	let intent: Intent | null = null;
	if (name === "beforeShellExecution") {
		intent = event.command ? { kind: "bash", command: event.command } : null;
	} else if (name === "beforeReadFile") {
		const p = event.path ?? event.file_path;
		intent = p ? { kind: "read", path: p } : null;
	} else if (name === "preToolUse") {
		// The matcher is left unset in hooks.json; the guard filters by tool here.
		const ti = event.tool_input ?? {};
		const tn = event.tool_name ?? "";
		const p = ti.path ?? ti.file_path;
		if ((tn === "Write" || tn === "Edit" || tn === "Delete") && p) {
			intent = { kind: "write", path: p };
		}
	}
	if (!intent) return;

	const verdict = evaluate(intent, { cwd, home: homedir(), projectRoot: workspaceRoot(cwd) });
	if (!verdict) return;

	// Ask-capable events can escalate to the user; the deny-only events degrade
	// every ask to deny. Unattended mode resolves asks without a human.
	let permission: "allow" | "ask" | "deny";
	if (verdict.action === "deny" || !ASK_CAPABLE.has(name)) {
		permission = "deny";
	} else if (!unattended) {
		permission = "ask";
	} else if (verdict.unattended === "allow") {
		permission = "allow";
	} else {
		permission = "deny";
	}
	const suffix =
		permission === "allow" ? " [unattended: pre-approved by rules.json]"
		: permission === "deny" && verdict.action === "ask" ? " [unattended: auto-denied — no user to ask]"
		: "";
	const message = `[${verdict.rule}] ${verdict.reason}${suffix}`;
	await Deno.stdout.write(
		new TextEncoder().encode(JSON.stringify({ permission, user_message: message, agent_message: message })),
	);
}

try {
	await main();
} catch (e) {
	console.error(`guard failed: ${e}`);
	Deno.exit(2);
}
