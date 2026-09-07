/**
 * Codex adapter — PreToolUse only.
 *
 * Codex has no ask verdict: returning one marks the hook failed AND lets the call
 * proceed (fail-open). So every ask degrades to deny+reason — the reason tells the
 * model to present the change and wait for approval. Only deny is ever emitted;
 * no verdict → silent exit 0 (Codex's normal flow). A crash exits 2 — deny.
 */

import { evaluate, workspaceRoot, type Intent } from "./core/mod.ts"; // paths are written against the deployed layout (~/.codex/hooks/) — guard.ts sits beside core/, not against this repo tree
import { homedir } from "node:os";

async function main() {
	const raw = await new Response(Deno.stdin.readable).text();
	const event = JSON.parse(raw) as {
		tool_name?: string;
		tool_input?: Record<string, string>;
		cwd?: string;
	};
	const name = event.tool_name ?? "";
	const input = event.tool_input ?? {};
	// Events normally carry cwd; the homedir fallback keeps the scope classes
	// meaningful if one ever omits it — a "/" fallback would exempt the whole
	// filesystem from them.
	const cwd = event.cwd ?? homedir();

	let intent: Intent | null = null;
	if (name === "Bash" || name === "exec_command") {
		intent = input.command ? { kind: "bash", command: input.command } : null;
	} else if (name === "apply_patch") {
		// Codex edits arrive as apply_patch command text; path scanning covers them.
		intent = input.command ? { kind: "bash", command: input.command } : null;
	}
	if (!intent) return;

	const verdict = evaluate(intent, { cwd, home: homedir(), projectRoot: workspaceRoot(cwd) });
	if (!verdict) return;

	await Deno.stdout.write(
		new TextEncoder().encode(
			JSON.stringify({
				hookSpecificOutput: {
					hookEventName: "PreToolUse",
					permissionDecision: "deny",
					permissionDecisionReason: `[${verdict.rule}] approval required — present the change to the user and wait. ${verdict.reason}`,
				},
			}),
		),
	);
}

try {
	await main();
} catch (e) {
	console.error(`guard failed: ${e}`);
	Deno.exit(2);
}
