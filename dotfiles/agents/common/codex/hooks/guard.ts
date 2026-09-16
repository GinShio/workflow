/**
 * Codex adapter — PreToolUse only.
 *
 * Runs under the host node — the same runtime the agent CLIs themselves need,
 * so the guard adds no dependency of its own (native TS stripping: node ≥ 22.18).
 *
 * Codex 0.150.x hooks have no usable ask or allow verdict: the binary rejects
 * both ("PreToolUse hook returned unsupported permissionDecision:ask/allow"),
 * and a failed hook is fail-open. So every ask degrades to deny+reason — the
 * reason tells the model to present the change and wait for approval — except
 * a verdict the table marked unattended.allow, which passes through silently
 * to the host's normal approval flow: its permission_mode / approval_policy
 * own attendance, and the guard neither allows nor blocks there. Only deny is
 * ever emitted; no verdict → silent exit 0 (Codex's normal flow). A crash
 * exits 2 — deny.
 *
 * Re-verify the degradation against each Codex upgrade: upstream main already
 * carries ask/allow in its PreToolUse output schema, so a newer Codex may
 * make real asks (and unattended allows) expressible here.
 *
 * Trust: user-layer hooks run only after the TUI trust flow, or
 * --dangerously-bypass-hook-trust for automation — an untrusted hook is listed
 * but never run. After deploying, verify the guard actually fires.
 */

import { evaluate, workspaceRoot, type Intent } from "./core/mod.ts"; // paths are written against the deployed layout (~/.codex/hooks/) — guard.ts sits beside core/, not against this repo tree
import { homedir } from "node:os";
import { readFileSync, writeSync } from "node:fs";
import process from "node:process";

function main() {
	const raw = readFileSync(0, "utf8");
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

	// A table pre-approval passes through to the host's approval flow, which
	// owns the decision (see header). Attended, that flow prompts the user;
	// unattended, its own policy resolves it — either way the guard stays out.
	if (verdict.action === "ask" && verdict.unattended === "allow") return;

	writeSync(
		1,
		JSON.stringify({
			hookSpecificOutput: {
				hookEventName: "PreToolUse",
				permissionDecision: "deny",
				permissionDecisionReason: `[${verdict.rule}] approval required — present the change to the user and wait. ${verdict.reason}`,
			},
		}),
	);
}

try {
	main();
} catch (e) {
	console.error(`guard failed: ${e}`);
	process.exit(2);
}
