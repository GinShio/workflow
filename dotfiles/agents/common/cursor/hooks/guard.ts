/**
 * Cursor adapter — one script for the gating events.
 *
 * beforeShellExecution and beforeMCPExecution can truly escalate to the user
 * (`permission: "ask"`); beforeReadFile and preToolUse are deny-only, so their
 * ask verdicts degrade to deny+reason — never a silent allow (preToolUse's ask
 * is accepted but unenforced, which amounts to allowing). beforeMCPExecution
 * carries unstructured MCP payloads: wired but unmapped in v1. A crash exits 2 —
 * Cursor's deny.
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

	const action = ASK_CAPABLE.has(name) ? verdict.action : "deny";
	const message = `[${verdict.rule}] ${verdict.reason}`;
	await Deno.stdout.write(
		new TextEncoder().encode(JSON.stringify({ permission: action, user_message: message, agent_message: message })),
	);
}

try {
	await main();
} catch (e) {
	console.error(`guard failed: ${e}`);
	Deno.exit(2);
}
