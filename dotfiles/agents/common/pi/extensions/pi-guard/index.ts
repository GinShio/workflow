/**
 * pi-guard — the working agreement's mechanical rules at pi's tool boundary.
 *
 * The shared rule table (hooks/rules.json, via hooks/core) decides; this extension
 * only translates pi's tool_call events into core intents and the verdict back into
 * pi's blocking model. pi's tool_call has no ask verdict, so `ask` escalates to the
 * user through ctx.ui.confirm — the confirm IS the approval gate. Refusal, missing
 * UI, or any guard failure blocks — never a silent allow.
 *
 * Unattended mode is the designed exception to unbounded confirms: `/unattended
 * on|off|status`, `pi --unattended`, or the cross-tool attendance signal
 * AGENTS_UNATTENDED=1 in the environment, forced on whenever there is no UI
 * (print / json). While it is on, ask verdicts resolve without waiting forever —
 *   - a verdict the table marked `unattended: "allow"` is approved and audited (the
 *     user's standing pre-approval; honored only while unattended);
 *   - headless asks deny immediately (no dialog anyone could answer);
 *   - TUI/RPC asks confirm with a countdown (rules.json unattended.timeoutMs) and
 *     deny at expiry, so a user returning mid-countdown can still answer — RPC
 *     dialogs with a timeout are auto-resolved agent-side, so RPC is bounded the
 *     same way (rpc.md, extension UI protocol).
 * Every unattended resolution is audited as a pi-guard-unattended-audit entry; the
 * mode itself persists as pi-guard-unattended-mode entries, the last one winning
 * across /tree, /fork, and resume. deny verdicts are untouched by any of this.
 *
 * Companion to pi-discipline (stateful session mechanics); kept separate because
 * policy enforcement and session state change on different cadences.
 */

import {
	isToolCallEventType,
	type ExtensionAPI,
	type ExtensionContext,
} from "@earendil-works/pi-coding-agent";
import type { AutocompleteItem } from "@earendil-works/pi-tui";
import { homedir } from "node:os";
import { evaluate, unattendedPolicy, workspaceRoot, type Intent } from "../../hooks/core/mod.ts"; // against the deployed layout: extensions/pi-guard/ is two levels under agent/, hooks/ deploys beside extensions/

const MODE_ENTRY_TYPE = "pi-guard-unattended-mode";
const AUDIT_ENTRY_TYPE = "pi-guard-unattended-audit";

/** Unattended mode is session state; the branch is the source of truth (last entry wins). */
let unattended = false;

/** How an ask resolved without a human approving it — the review surface. */
type AuditHow = "policy" | "no-ui" | "timeout-or-cancel" | "dialog-failure";

export default function (pi: ExtensionAPI) {
	/** Mirror the mode into the footer so a leaving user sees what they armed. */
	function reflectMode(ctx: ExtensionContext): void {
		if (!ctx.hasUI) return;
		ctx.ui.setStatus("pi-guard", unattended ? "unattended" : undefined);
	}

	/** Rebuild the mode from the branch: later entries win, unrecognized shapes are skipped. */
	function reconstructMode(ctx: ExtensionContext): void {
		unattended = false;
		for (const entry of ctx.sessionManager.getBranch()) {
			if (entry.type !== "custom" || entry.customType !== MODE_ENTRY_TYPE) continue;
			const data = entry.data as { on?: boolean } | undefined;
			if (data && typeof data.on === "boolean") unattended = data.on;
		}
	}

	/**
	 * Persist an unattended resolution. Custom entries stay out of the LLM context;
	 * the session JSONL is the review surface (no viewer command — second-case trigger).
	 */
	function audit(
		ctx: ExtensionContext,
		intent: Intent,
		rule: string,
		resolution: "allow" | "deny",
		how: AuditHow,
	): void {
		const subject = intent.kind === "bash" ? intent.command : intent.path;
		pi.appendEntry(AUDIT_ENTRY_TYPE, {
			rule,
			kind: intent.kind,
			subject,
			resolution,
			how,
			timestamp: Date.now(),
		});
		if (ctx.hasUI) {
			const shown = subject.length > 120 ? `${subject.slice(0, 117)}…` : subject;
			ctx.ui.notify(`[pi-guard] unattended ${resolution} [${rule}] ${shown}`, "warning");
		}
	}

	pi.registerFlag("unattended", {
		description:
			"Start with pi-guard unattended mode on: ask verdicts resolve from the table's unattended policy instead of an unbounded confirm",
		type: "boolean",
		default: false,
	});

	pi.on("session_start", (_event, ctx) => {
		reconstructMode(ctx);
		// An explicit launch flag is this run's intent: it overrides whatever the
		// session branch recorded, and is re-armed so the mode survives later
		// resumes of the same session, where the flag is absent.
		if (pi.getFlag("unattended") && !unattended) {
			unattended = true;
			pi.appendEntry(MODE_ENTRY_TYPE, { on: true, source: "flag" });
		}
		// The cross-tool attendance signal: the subprocess adapters read the same
		// variable (they cannot prompt or arm a mode mid-session), so one launcher
		// env covers every host. pi honors it exactly like the flag.
		if (!unattended && Deno.env.get("AGENTS_UNATTENDED") === "1") {
			unattended = true;
			pi.appendEntry(MODE_ENTRY_TYPE, { on: true, source: "env" });
		}
		reflectMode(ctx);
	});

	pi.on("session_tree", (_event, ctx) => {
		reconstructMode(ctx);
		reflectMode(ctx); // the navigated branch point may hold a different mode — keep the footer honest
	});

	pi.registerCommand("unattended", {
		description: "pi-guard unattended mode: on | off | status",
		getArgumentCompletions: (prefix: string): AutocompleteItem[] | null => {
			const options = ["on", "off", "status"].filter((value) => value.startsWith(prefix));
			return options.length > 0 ? options.map((value) => ({ value, label: value })) : null;
		},
		handler: async (args, ctx) => {
			const arg = args.trim().toLowerCase();
			if (arg === "" || arg === "status") {
				ctx.ui.notify(
					`pi-guard unattended: ${unattended ? "on" : "off"}${!ctx.hasUI ? " (forced on: no UI)" : ""}`,
					"info",
				);
				return;
			}
			if (arg !== "on" && arg !== "off") {
				ctx.ui.notify("usage: /unattended on|off|status", "warning");
				return;
			}
			unattended = arg === "on";
			pi.appendEntry(MODE_ENTRY_TYPE, { on: unattended, source: "command" });
			reflectMode(ctx);
			ctx.ui.notify(`pi-guard unattended: ${unattended ? "on" : "off"}`, "info");
		},
	});

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

		// ask verdicts resolve by attendance. Headless (print/json) counts as
		// unattended: nobody can answer a dialog there, now or later.
		const unattendedNow = unattended || !ctx.hasUI;
		const policy = unattendedPolicy();

		// Standing pre-approval transcribed by the table — honored only unattended.
		if (unattendedNow && verdict.unattended === "allow") {
			audit(ctx, intent, verdict.rule, "allow", "policy");
			return undefined;
		}

		// Nobody can answer a dialog headless, and a 0 timeout promises deny
		// without waiting — pi treats a 0 timeout option as absent (falsy), so
		// handing the dialog { timeout: 0 } would wait forever. Both deny now,
		// before any dialog is shown (audit channel: no dialog took place).
		if (unattendedNow && (!ctx.hasUI || policy.timeoutMs === 0)) {
			const why = ctx.hasUI ? "timeout policy is 0" : "no UI to ask";
			audit(ctx, intent, verdict.rule, "deny", "no-ui");
			return { block: true, reason: `${reason} [unattended: auto-denied — ${why}]` };
		}

		let approved = false;
		let dialogFailed = false;
		try {
			approved = await ctx.ui.confirm(
				`[${verdict.rule}] approval needed${unattendedNow ? ` — auto-deny in ${Math.round(policy.timeoutMs / 1000)}s` : ""}`,
				verdict.reason,
				unattendedNow ? { timeout: policy.timeoutMs } : undefined,
			);
		} catch {
			dialogFailed = true; // fail toward blocking, never allowing
		}
		if (unattendedNow && !approved) {
			// The timeout option cannot distinguish expiry from Esc; both deny. A
			// thrown dialog never resolved at all — audited under its own how so
			// the session JSONL separates crashed dialogs from timed-out ones.
			audit(ctx, intent, verdict.rule, "deny", dialogFailed ? "dialog-failure" : "timeout-or-cancel");
			return { block: true, reason: `${reason} [unattended: auto-denied — timeout or cancel]` };
		}
		return approved ? undefined : { block: true, reason };
	});
}
