/**
 * Claude Code PreToolUse adapter — one event on stdin, one decision JSON on stdout.
 *
 * deny → "deny"; ask → "ask" (the native permission prompt); no verdict → silent
 * exit 0, the normal permission flow decides. Unattended runs (AGENTS_UNATTENDED=1
 * in the environment, exported by the launcher when nobody will answer prompts)
 * resolve asks without a human: a verdict the table marked unattended.allow
 * becomes "allow" — the user's standing pre-approval, honored only unattended —
 * and any other ask becomes "deny", because headless asks have no one to answer
 * them. Attended behavior is unchanged, and outside unattended mode the guard
 * never allows. A crash denies with a guard-failed reason: a broken guard
 * blocks, never passes.
 *
 * Host guard — this adapter answers Claude Code only. Cursor's third-party hook
 * compatibility loads ~/.claude/settings.json and runs whatever it finds there
 * against Cursor's own event stream (cursor.com/docs/reference/third-party-hooks),
 * where ~/.cursor/hooks.json already runs the adapter written for it. Two
 * adapters with two degradation models deciding one action is not redundancy:
 * this one degrades against Claude Code's event set and permission vocabulary,
 * and Cursor's compatibility layer rewrites neither. So it stands down when the
 * payload is not Claude Code's — see isForeignHost.
 */

import { evaluate, isUnattended, workspaceRoot, type Intent } from "./core/mod.ts"; // paths are written against the deployed layout (~/.claude/hooks/) — guard.ts sits beside core/, not against this repo tree
import { homedir } from "node:os";

const OUT = new TextEncoder();

/**
 * Whether this PreToolUse payload came from a host other than Claude Code.
 *
 * `cursor_version` sits in Cursor's common hook input — every event, every
 * source, including the Claude configs it imports — and in no Claude Code
 * payload. Claude Code's own `hook_event_name` is "PreToolUse"; Cursor rewrites
 * it to its own "preToolUse" before dispatch, so either field identifies the
 * foreign host. Both are read from stdin the adapter already parses, which is
 * why neither is the CURSOR_VERSION environment variable Cursor also sets: an
 * env read needs a permission grant that may not arrive, and its absence would
 * throw into the crash handler — the very failure this stand-down exists to end.
 */
function isForeignHost(event: { cursor_version?: string; hook_event_name?: string }): boolean {
	return event.cursor_version !== undefined ||
		(event.hook_event_name !== undefined && event.hook_event_name !== "PreToolUse");
}

async function main() {
	const raw = await new Response(Deno.stdin.readable).text();
	const event = JSON.parse(raw) as {
		tool_name?: string;
		tool_input?: Record<string, string>;
		cwd?: string;
		cursor_version?: string;
		hook_event_name?: string;
	};
	if (isForeignHost(event)) return;
	const input = event.tool_input ?? {};
	const name = event.tool_name ?? "";
	// Events normally carry cwd; the homedir fallback keeps the scope classes
	// meaningful if one ever omits it — a "/" fallback would exempt the whole
	// filesystem from them.
	const cwd = event.cwd ?? homedir();
	const unattended = isUnattended();

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

	let permission: "allow" | "ask" | "deny";
	let reason = `[${verdict.rule}] ${verdict.reason}`;
	if (verdict.action === "deny") {
		permission = "deny";
	} else if (!unattended) {
		permission = "ask";
	} else if (verdict.unattended === "allow") {
		permission = "allow";
		reason += " [unattended: pre-approved by rules.json]";
	} else {
		permission = "deny";
		reason += " [unattended: auto-denied — no user to ask]";
	}

	await Deno.stdout.write(
		OUT.encode(
			JSON.stringify({
				hookSpecificOutput: {
					hookEventName: "PreToolUse",
					permissionDecision: permission,
					permissionDecisionReason: reason,
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
