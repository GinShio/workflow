/**
 * pi-guard — the working agreement's mechanical rules at pi's tool boundary.
 *
 * Stateless: the shared rule table (hooks/rules.json, via hooks/core) decides;
 * this extension only translates pi's tool_call events into core intents and the
 * verdict back into pi's blocking model. pi's tool_call has no ask verdict, so
 * `ask` escalates to the user through ctx.ui.confirm — the confirm IS the approval
 * gate. Refusal, missing UI, or any guard failure blocks — never a silent allow.
 *
 * Companion to pi-discipline (stateful session mechanics); kept separate because
 * policy enforcement and session state change on different cadences.
 */

import { isToolCallEventType, type ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { homedir } from "node:os";
import { evaluate, workspaceRoot, type Intent } from "../../hooks/core/mod.ts"; // against the deployed layout: extensions/pi-guard/ is two levels under agent/, hooks/ deploys beside extensions/

export default function (pi: ExtensionAPI) {
	pi.on("tool_call", async (event, ctx) => {
		let intent: Intent | null = null;
		if (isToolCallEventType("bash", event)) {
			intent = event.input.command ? { kind: "bash", command: event.input.command } : null;
		} else if (isToolCallEventType("read", event)) {
			intent = { kind: "read", path: event.input.path };
		} else if (isToolCallEventType("write", event)) {
			intent = { kind: "write", path: event.input.path };
		} else if (isToolCallEventType("edit", event)) {
			// pi's edit tool may batch several edits to one path — one gate per call.
			intent = { kind: "write", path: event.input.path };
		}
		if (!intent) return undefined;

		const verdict = evaluate(intent, {
			cwd: ctx.cwd,
			home: homedir(),
			projectRoot: workspaceRoot(ctx.cwd),
		});
		if (!verdict) return undefined;

		const reason = `[${verdict.rule}] ${verdict.reason}`;
		if (verdict.action === "deny") return { block: true, reason };
		let approved = false;
		try {
			approved = await ctx.ui.confirm(`[${verdict.rule}] approval needed`, verdict.reason);
		} catch {
			approved = false; // no UI — fail toward blocking, never allowing
		}
		return approved ? undefined : { block: true, reason };
	});
}
