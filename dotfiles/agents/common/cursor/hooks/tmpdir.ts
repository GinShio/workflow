/**
 * Cursor adapter — TMPDIR for agent shell commands.
 *
 * Runs under the host node, like guard.ts (native TS stripping: node ≥ 22.18).
 *
 * Cursor has no setting that reaches the agent's shell environment — sessionStart's
 * `env` output reaches only later hook executions (cursor.com/docs/agent/hooks) —
 * so preToolUse does what pi's shellCommandPrefix does: `updated_input` prepends an
 * export of the per-agent scratch dir to every Shell command. hooks.json passes that
 * dir as the sole argument, rendered from the same template value as the tmpfiles
 * entry that creates it and the rules.json prefixes that exempt it.
 *
 * Under Cursor's own sandbox the export stands aside: the sandbox hands each command
 * a TMPDIR of its own and drops XDG_RUNTIME_DIR from its environment, so the scratch
 * dir lies outside what it prepares (cursor-agent 2026.10.01). CURSOR_SANDBOX marks
 * exactly those commands; Cursor keeps it out of the shell state it carries from one
 * command to the next.
 *
 * The response never carries `permission` — approval stays with guard.ts and
 * Cursor — yet each answer is a JSON object, since preToolUse blocks the tool call
 * on empty or invalid output. A crash exits 1 with no answer, which Cursor fails
 * open: the command runs with whatever TMPDIR its shell already holds.
 */

import { readFileSync, writeSync } from "node:fs";
import process from "node:process";

function shellQuote(s: string): string {
	return `'${s.replaceAll("'", `'\\''`)}'`;
}

function main() {
	const scratch = process.argv[2];
	if (!scratch) throw new Error("expected the scratch dir as the only argument");
	const event = JSON.parse(readFileSync(0, "utf8")) as {
		tool_name?: string;
		tool_input?: Record<string, unknown>;
	};
	const input = event.tool_input ?? {};
	// hooks.json narrows the matcher to Shell; the test is repeated because the
	// matcher lives in a different file and Cursor merges four config sources.
	if (event.tool_name !== "Shell" || typeof input.command !== "string") {
		writeSync(1, "{}");
		return;
	}
	const prefix = `[ -n "\${CURSOR_SANDBOX-}" ] || export TMPDIR=${shellQuote(scratch)}`;
	// The whole input, not just command: the docs define updated_input as the
	// input to use instead.
	writeSync(1, JSON.stringify({ updated_input: { ...input, command: `${prefix}\n${input.command}` } }));
}

try {
	main();
} catch (e) {
	console.error(`tmpdir hook failed: ${e}`);
	process.exit(1);
}
