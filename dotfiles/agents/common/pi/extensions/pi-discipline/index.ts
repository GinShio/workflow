/**
 * pi-discipline — the session's working-agreement mechanics.
 *
 * Two tools, each owning one tracked state:
 *
 * - `todo` — the session task plan. Each task carries a verification criterion (the check
 *   that proves it done); every call re-renders the whole plan, so a long session keeps
 *   re-grounding on it instead of drifting. A mutation render is new state by definition,
 *   so it is never suppressed. State lives in the tool result details and is rebuilt from
 *   the session branch, so /tree, /fork, and compaction keep it consistent with history.
 *
 * - `protocol` — loads a working-agreement skill (design-protocol, implementation-protocol,
 *   review-protocol, ...) into the conversation at a phase transition; the load is the
 *   phase transition. The system prompt only carries skill descriptions; this makes
 *   loading the full text a first-class tool action instead of a remembered file read.
 *   The skill text is constant for the session, so a call whose full text is still
 *   verbatim in context returns empty — a second copy carries zero information.
 *
 * Content-identity rule, shared by both tools: return nothing when what would be
 * returned is already verbatim in the kept window; return it in full otherwise. The
 * kept window is derived from the branch itself — the latest compaction or
 * branch-summary anchor onward — so no state has to survive /tree, /fork, or compaction.
 *
 * Compaction visibility: a compaction keeps tracked state only as a paraphrase inside
 * the summary, so the extension re-injects the plan and/or the governing protocol —
 * verbatim, with a minimal attribution label — when their latest full render fell
 * outside the kept window. Plan and phase sections stitch into one message.
 *
 * No npm dependencies — everything imported here is provided by pi's runtime.
 */

import { StringEnum } from "@earendil-works/pi-ai";
import {
	stripFrontmatter,
	type ExtensionAPI,
	type ExtensionContext,
	type SessionEntry,
	type Skill,
} from "@earendil-works/pi-coding-agent";
import { Text } from "@earendil-works/pi-tui";
import { Type } from "typebox";
import { readFileSync } from "node:fs";

const PLAN_MESSAGE_TYPE = "pi-discipline-plan";

// ---------------------------------------------------------------------------
// task plan state
// ---------------------------------------------------------------------------

type TaskStatus = "pending" | "in_progress" | "done";

interface Task {
	id: number;
	subject: string;
	criterion?: string;
	status: TaskStatus;
}

interface PlanDetails {
	action: "add" | "update" | "list";
	tasks: Task[];
	nextId: number;
	error?: string;
}

interface ProtocolDetails {
	loaded: boolean;
	name?: string;
	path?: string;
	available?: string[];
	/** True when the call returned empty because this skill's full text is still in context. */
	deduped?: boolean;
}

/** Sections carried by a re-injection message (which states it re-injected, and which phase). */
interface ReinjectionDetails {
	plan?: boolean;
	protocol?: string;
}

let tasks: Task[] = [];
let nextId = 1;
let sessionSkills: Skill[] = [];

/** The LLM-facing plan text — the whole plan on every call, so attention re-anchors. */
function renderPlanText(): string {
	if (tasks.length === 0) {
		return (
			"Plan is empty. For multi-step or multi-file work, add tasks first — each with a " +
			"verification criterion: the concrete check that proves it done (usually a command " +
			"and the output that counts as green). Work one task at a time."
		);
	}
	const done = tasks.filter((t) => t.status === "done").length;
	const lines: string[] = [`Plan: ${done}/${tasks.length} done`];
	for (const t of tasks) {
		const mark = t.status === "done" ? "x" : t.status === "in_progress" ? ">" : " ";
		const criterion = t.criterion ? ` — verify: ${t.criterion}` : " — (no criterion)";
		lines.push(`[${mark}] #${t.id} ${t.subject}${criterion}`);
	}
	const current = tasks.find((t) => t.status === "in_progress");
	if (current) {
		lines.push(
			`Current task: #${current.id} ${current.subject} — close it by running its criterion before opening the next.`,
		);
	} else {
		const nextTask = tasks.find((t) => t.status === "pending");
		if (nextTask) {
			lines.push(`No task in progress. Next: #${nextTask.id} ${nextTask.subject}.`);
		}
	}
	return lines.join("\n");
}

function planReply(notes: string[], action: PlanDetails["action"]) {
	const text = notes.length > 0 ? `${notes.join("\n")}\n\n${renderPlanText()}` : renderPlanText();
	return {
		content: [{ type: "text" as const, text }],
		details: { action, tasks: tasks.map((t) => ({ ...t })), nextId } satisfies PlanDetails,
	};
}

function planError(error: string, action: PlanDetails["action"]) {
	return {
		content: [{ type: "text" as const, text: `Error: ${error}` }],
		details: { action, tasks: tasks.map((t) => ({ ...t })), nextId, error } satisfies PlanDetails,
	};
}

/** Rebuild plan state from the current session branch (latest cumulative details win). */
function reconstructState(ctx: ExtensionContext) {
	tasks = [];
	nextId = 1;
	for (const entry of ctx.sessionManager.getBranch()) {
		if (entry.type !== "message") continue;
		const msg = entry.message;
		if (msg.role !== "toolResult" || msg.toolName !== "todo") continue;
		const details = msg.details as PlanDetails | undefined;
		// Details replay across extension versions; an unrecognized shape degrades
		// to the last understood state instead of throwing mid-replay.
		if (details && Array.isArray(details.tasks)) {
			// Copy: later status updates must not mutate the entry objects that
			// older branches read their state from (/tree, /fork replay).
			tasks = details.tasks.map((t) => ({ ...t }));
			nextId = details.nextId;
		}
	}
}

// ---------------------------------------------------------------------------
// kept window
// ---------------------------------------------------------------------------

/**
 * Branch index where the kept window starts: entries at or after it are verbatim in
 * the LLM context, entries before it survive only as summary paraphrase. Derived from
 * the branch itself — the latest compaction or branch-summary anchor onward, per
 * buildContextEntries — so no state has to survive /tree, /fork, or compaction.
 *
 * An unresolvable anchor degrades to the boundary entry's own index. That can only
 * shrink the window, which errs toward a redundant full insert — never a silently
 * missing one.
 */
function windowStartIndex(branch: SessionEntry[]): number {
	let start = 0;
	for (let i = 0; i < branch.length; i++) {
		const entry = branch[i];
		if (entry.type === "compaction") {
			const j = branch.findIndex((e) => e.id === entry.firstKeptEntryId);
			start = Math.max(start, j >= 0 ? j : i);
		} else if (entry.type === "branch_summary") {
			const j = branch.findIndex((e) => e.id === entry.fromId);
			start = Math.max(start, j >= 0 ? j : i);
		}
	}
	return start;
}

/**
 * Last full render of a tracked state — a tool result that inserted the text, or a
 * re-injection message carrying it. Deduped protocol calls don't count: their empty
 * result holds no text. `skill` restricts protocol renders to one skill; todo renders
 * are skill-independent.
 */
function lastRenderIndex(
	branch: SessionEntry[],
	kind: "todo" | "protocol",
	skill?: string,
): number {
	return branch.findLastIndex((entry) => {
		if (entry.type === "message") {
			const msg = entry.message;
			if (msg.role !== "toolResult" || msg.toolName !== kind) return false;
			if (kind === "protocol") {
				const details = msg.details as ProtocolDetails | undefined;
				return details?.loaded === true && !details.deduped && details.name === skill;
			}
			const details = msg.details as PlanDetails | undefined;
			return !!details && Array.isArray(details.tasks);
		}
		if (entry.type === "custom_message" && entry.customType === PLAN_MESSAGE_TYPE) {
			// Re-injections count as renders: the next compaction must not duplicate
			// text a previous one still keeps in context. Messages from before
			// ReinjectionDetails existed carried only the plan.
			const details = entry.details as ReinjectionDetails | undefined;
			if (!details) return kind === "todo";
			return kind === "todo" ? details.plan === true : details.protocol === skill;
		}
		return false;
	});
}

/** The governing protocol — the last protocol call that resolved and loaded a skill. */
function lastPhase(branch: SessionEntry[]): { name: string; path: string } | undefined {
	for (let i = branch.length - 1; i >= 0; i--) {
		const entry = branch[i];
		if (entry.type !== "message") continue;
		const msg = entry.message;
		if (msg.role !== "toolResult" || msg.toolName !== "protocol") continue;
		const details = msg.details as ProtocolDetails | undefined;
		if (details?.loaded && details.name && details.path) {
			return { name: details.name, path: details.path };
		}
	}
	return undefined;
}

// ---------------------------------------------------------------------------
// tool parameter schemas
// ---------------------------------------------------------------------------

const TaskInput = Type.Object({
	subject: Type.String({
		description: "What the task does, one line, in execution order",
	}),
	criterion: Type.Optional(
		Type.String({
			description:
				"The verification criterion: the concrete check that proves this task done — usually the command and the output that counts as green",
		}),
	),
});

const TodoParams = Type.Object({
	action: StringEnum(["add", "update", "list"] as const, {
		description: "add: append tasks; update: set one task's status; list: show the plan",
	}),
	tasks: Type.Optional(Type.Array(TaskInput, { description: "Tasks to append (action=add)" })),
	id: Type.Optional(Type.Number({ description: "Task id (action=update)" })),
	status: Type.Optional(
		StringEnum(["pending", "in_progress", "done"] as const, {
			description:
				"New status (action=update). done only after the task's criterion actually ran green",
		}),
	),
});

const ProtocolParams = Type.Object({
	skill: Type.String({
		description:
			"Name of the skill to load, e.g. design-protocol, implementation-protocol, review-protocol",
	}),
});

// ---------------------------------------------------------------------------
// extension
// ---------------------------------------------------------------------------

export default function (pi: ExtensionAPI) {
	pi.on("session_start", (_event, ctx) => reconstructState(ctx));
	pi.on("session_tree", (_event, ctx) => reconstructState(ctx));

	// Skills visible to the session, refreshed on every agent run so the protocol
	// tool resolves against pi's own discovery (settings, project dirs, CLI).
	pi.on("before_agent_start", (event) => {
		sessionSkills = event.systemPromptOptions.skills ?? [];
	});

	// Compaction keeps tracked state only as a paraphrase inside the summary (Progress /
	// Next Steps); the verbatim text drops out when the latest render falls outside the
	// kept window. Inject exactly then — not when the latest render already survives
	// (kept-window duplication is redundant, not harmful). Plan and phase sections
	// stitch into one message.
	pi.on("session_compact", (event, ctx) => {
		const branch = ctx.sessionManager.getBranch();
		const keptIndex = branch.findIndex(
			(entry) => entry.id === event.compactionEntry.firstKeptEntryId,
		);
		// pi never cuts at a tool result, so the last render is either fully kept or
		// fully summarized and the index comparison is well-defined. An unresolvable
		// kept id (-1) skips the injection: pi's fallback keeps the recent window, and
		// a missed injection is recoverable via todo list and protocol.
		if (keptIndex < 0) return;
		const sections: string[] = [];
		const details: ReinjectionDetails = {};
		if (tasks.length > 0) {
			const lastTodo = lastRenderIndex(branch, "todo");
			if (lastTodo < 0 || lastTodo < keptIndex) {
				sections.push(`Task plan (re-injected after compaction):\n\n${renderPlanText()}`);
				details.plan = true;
			}
		}
		const phase = lastPhase(branch);
		if (phase) {
			const lastFull = lastRenderIndex(branch, "protocol", phase.name);
			if (lastFull < 0 || lastFull < keptIndex) {
				// Re-read so the reinjection carries the current text; a failed read
				// skips the section (recoverable: the model can call protocol).
				try {
					const body = readFileSync(phase.path, "utf8");
					sections.push(
						`${phase.name} (re-injected after compaction):\n\n${stripFrontmatter(body)}`,
					);
					details.protocol = phase.name;
				} catch {
					// Unreadable file — skip the section.
				}
			}
		}
		if (sections.length === 0) return;
		pi.sendMessage(
			{
				customType: PLAN_MESSAGE_TYPE,
				content: sections.join("\n\n"),
				display: true,
				details,
			},
			{ deliverAs: ctx.isIdle() ? "nextTurn" : "steer" },
		);
	});

	pi.registerTool({
		name: "todo",
		label: "Todo",
		description:
			"Session task plan for multi-step work. Each task carries a verification criterion — " +
			"the concrete check (usually a command and its green output) that proves it done. " +
			"Actions: add (tasks), update (id + status), list. Work one task at a time: set it " +
			"in_progress, close it by running its criterion, then mark it done and open the next. " +
			"Every reply re-renders the whole plan.",
		promptSnippet: "Plan multi-step work as tasks, each with a verification criterion",
		promptGuidelines: [
			"Use todo to plan multi-phase or multi-file work before the first edit, and close a task only after its verification criterion has run green.",
		],
		parameters: TodoParams,

		async execute(_toolCallId, params, _signal, _onUpdate, _ctx) {
			// Synchronous state transitions: each call is atomic on the event loop,
			// so sibling parallel calls cannot interleave mid-update.
			switch (params.action) {
				case "list":
					return planReply([], "list");

				case "add": {
					if (!params.tasks || params.tasks.length === 0) {
						return planError("add requires `tasks`", "add");
					}
					const notes: string[] = [];
					for (const t of params.tasks) {
						tasks.push({ id: nextId, subject: t.subject, criterion: t.criterion, status: "pending" });
						if (!t.criterion) {
							notes.push(`#${nextId} has no verification criterion — it cannot be closed objectively.`);
						}
						nextId++;
					}
					return planReply(notes, "add");
				}

				case "update": {
					if (params.id === undefined || !params.status) {
						return planError("update requires `id` and `status`", "update");
					}
					const task = tasks.find((t) => t.id === params.id);
					if (!task) {
						return planError(`task #${params.id} not found`, "update");
					}
					task.status = params.status;
					const notes: string[] = [];
					if (params.status === "done") {
						if (task.criterion) {
							notes.push(`#${task.id} criterion: ${task.criterion}`);
							notes.push("done requires that check to have run green; if it has not, keep the task in_progress and run it first.");
						} else {
							notes.push(`#${task.id} has no verification criterion — done cannot be evidenced for it.`);
						}
					}
					return planReply(notes, "update");
				}
			}
		},

		renderCall(args, theme, _context) {
			let text = theme.fg("toolTitle", theme.bold("todo ")) + theme.fg("muted", args.action);
			if (args.action === "add" && args.tasks) {
				text += theme.fg("dim", ` ${args.tasks.length} task(s)`);
			}
			if (args.action === "update" && args.id !== undefined) {
				text += theme.fg("accent", ` #${args.id}`);
				if (args.status) text += theme.fg("dim", ` → ${args.status}`);
			}
			return new Text(text, 0, 0);
		},

		renderResult(result, { expanded }, theme, _context) {
			const details = result.details as PlanDetails | undefined;
			if (!details) {
				const first = result.content[0];
				return new Text(first?.type === "text" ? first.text : "", 0, 0);
			}
			if (details.error) {
				return new Text(theme.fg("error", `Error: ${details.error}`), 0, 0);
			}
			const ts = details.tasks;
			if (ts.length === 0) {
				return new Text(theme.fg("dim", "Plan is empty"), 0, 0);
			}
			const done = ts.filter((t) => t.status === "done").length;
			let out = theme.fg("muted", `Plan ${done}/${ts.length} done`);
			const visible = expanded ? ts : ts.slice(0, 6);
			for (const t of visible) {
				const mark =
					t.status === "done"
						? theme.fg("success", "✓")
						: t.status === "in_progress"
							? theme.fg("accent", "►")
							: theme.fg("dim", "○");
				const subject = t.status === "done" ? theme.fg("dim", t.subject) : t.subject;
				out += `\n${mark} ${theme.fg("accent", `#${t.id}`)} ${subject}`;
				if (expanded && t.criterion) {
					out += theme.fg("dim", ` — ${t.criterion}`);
				}
			}
			if (!expanded && ts.length > 6) {
				out += theme.fg("dim", `\n… ${ts.length - 6} more`);
			}
			return new Text(out, 0, 0);
		},
	});

	pi.registerTool({
		name: "protocol",
		label: "Protocol",
		description:
			"Load a working-agreement skill's full text into the conversation — design-protocol " +
			"before designing, implementation-protocol before implementing, review-protocol before " +
			"reviewing or handing over. Call it at each phase transition, and mid-phase whenever " +
			"the protocol's rules are needed. Returns empty when this protocol's full text is " +
			"already in context.",
		promptSnippet: "Load a working-agreement skill's full text at a phase transition",
		parameters: ProtocolParams,

		async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
			const skill = sessionSkills.find(
				(s) => s.name === params.skill && !s.disableModelInvocation,
			);
			if (!skill) {
				const available = sessionSkills
					.filter((s) => !s.disableModelInvocation)
					.map((s) => s.name)
					.sort();
				return {
					content: [
						{
							type: "text" as const,
							text: `No skill named "${params.skill}". Available: ${available.join(", ") || "(none loaded)"}`,
						},
					],
					details: { loaded: false, available } satisfies ProtocolDetails,
				};
			}
			// Content-identity: the skill text is constant for the session, so if this
			// skill's full text is still verbatim in the kept window, a second copy
			// carries zero information — return empty. Re-anchoring stays available
			// through the native `read` path.
			const branch = ctx.sessionManager.getBranch();
			const lastFull = lastRenderIndex(branch, "protocol", skill.name);
			if (lastFull >= 0 && lastFull >= windowStartIndex(branch)) {
				return {
					content: [],
					details: {
						loaded: true,
						name: skill.name,
						path: skill.filePath,
						deduped: true,
					} satisfies ProtocolDetails,
				};
			}
			let body: string;
			try {
				body = readFileSync(skill.filePath, "utf8");
			} catch (e) {
				return {
					content: [{ type: "text" as const, text: `Could not read ${skill.filePath}: ${e}` }],
					details: { loaded: false, name: skill.name } satisfies ProtocolDetails,
				};
			}
			return {
				content: [
					{
						type: "text" as const,
						text: stripFrontmatter(body),
					},
				],
				details: { loaded: true, name: skill.name, path: skill.filePath } satisfies ProtocolDetails,
			};
		},

		renderCall(args, theme, _context) {
			return new Text(
				theme.fg("toolTitle", theme.bold("protocol ")) + theme.fg("muted", args.skill),
				0,
				0,
			);
		},

		renderResult(result, _options, theme, _context) {
			const details = result.details as ProtocolDetails | undefined;
			if (details?.loaded) {
				if (details.deduped) {
					return new Text(theme.fg("dim", `${details.name} — already in context`), 0, 0);
				}
				return new Text(
					theme.fg("success", "✓ ") +
						theme.fg("muted", `loaded ${details.name} — follow it for the current phase`),
					0,
					0,
				);
			}
			const first = result.content[0];
			const msg = first?.type === "text" ? first.text.split("\n")[0] : "failed to load";
			return new Text(theme.fg("error", msg), 0, 0);
		},
	});
}
