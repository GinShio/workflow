/**
 * Claude Code PreToolUse adapter — one event on stdin, one decision JSON on stdout.
 *
 * ask → permissionDecision "ask" (the native permission prompt); deny → "deny"
 * (reason shown to the model); no verdict → silent exit 0, the normal permission
 * flow decides — the guard never allows. A crash denies with a guard-failed
 * reason: a broken guard blocks, never passes.
 */

import { evaluate, workspaceRoot, type Intent } from "./core/mod.ts"; // paths are written against the deployed layout (~/.claude/hooks/) — guard.ts sits beside core/, not against this repo tree
import { homedir } from "node:os";

const OUT = new TextEncoder();

async function main() {
	const raw = await new Response(Deno.stdin.readable).text();
	const event = JSON.parse(raw) as {
		tool_name?: string;
		tool_input?: Record<string, string>;
		cwd?: string;
	};
	const input = event.tool_input ?? {};
	const name = event.tool_name ?? "";
	// Events normally carry cwd; the homedir fallback keeps the scope classes
	// meaningful if one ever omits it — a "/" fallback would exempt the whole
	// filesystem from them.
	const cwd = event.cwd ?? homedir();

	let intent: Intent | null = null;
	if (name === "Bash") {
		intent = input.command ? { kind: "bash", command: input.command } : null;
	} else if (name === "Read") {
		intent = input.file_path ? { kind: "read", path: input.file_path } : null;
	} else if (name === "Edit" || name === "Write") {
		// Path permission rules only consult Edit/Read in Claude Code — this
		// adapter is the enforcement point for Write too.
		intent = input.file_path ? { kind: "write", path: input.file_path } : null;
	} else if (name === "NotebookEdit") {
		intent = input.notebook_path ? { kind: "write", path: input.notebook_path } : null;
	}
	if (!intent) return;

	const verdict = evaluate(intent, { cwd, home: homedir(), projectRoot: workspaceRoot(cwd) });
	if (!verdict) return;

	await Deno.stdout.write(
		OUT.encode(
			JSON.stringify({
				hookSpecificOutput: {
					hookEventName: "PreToolUse",
					permissionDecision: verdict.action,
					permissionDecisionReason: `[${verdict.rule}] ${verdict.reason}`,
				},
			}),
		),
	);
}

try {
	await main();
} catch (e) {
	await Deno.stdout.write(
		OUT.encode(
			JSON.stringify({
				hookSpecificOutput: {
					hookEventName: "PreToolUse",
					permissionDecision: "deny",
					permissionDecisionReason: `[guard] guard failed (${e}) — treat as needing approval`,
				},
			}),
		),
	);
}
