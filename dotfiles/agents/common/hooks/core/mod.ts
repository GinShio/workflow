/**
 * hooks/core — the portable matcher behind every tool's guard adapter.
 *
 * One rule table (../rules.json), one verdict model: `deny` beats `ask`, and no
 * match returns null. The core never emits `allow` — allowing is the absence of a
 * verdict, so a guard that fails to match can never waive a rule.
 *
 * Tier doctrine — how a class earns its action. Two axes decide: blast radius
 * and recoverability (irreversible? machine-scoped?) against frequency and
 * reviewability (does agent work legitimately need it often? can the user judge
 * it from the command line?). Common + recoverable-in-tree is no class at all
 * (git is the recovery path); rare + irreversible/machine-scoped — or anything
 * crossing a trust boundary (publish, privilege, secrets and private stores) —
 * is deny; everything reviewable-with-one-approval is ask. New classes take
 * this test, not enumeration instinct.
 *
 * Reason doctrine — what a reason says, and to whom. The verdict's action
 * carries the gate semantics (deny stops the call; ask opens the approval
 * dialog) and the triggering command is in the model's context, but on every
 * block path the reason is the model's only instruction: deny reaches just
 * the model, and an ask — user-facing first — returns its reason to the model
 * on denial, on unattended timeout, and on hosts that degrade asks to deny.
 * So every reason is two clauses, rationale — next move. The rationale is
 * what the model cannot infer (irreversibility, trust boundary, ownership,
 * scope); the next move is one of three forms: the sanctioned alternative, the
 * hand-off ("hand the exact command to the user and wait"), or the stop
 * ("report; nothing here is sanctioned"). A rationale alone is a label — it
 * defers the next move to the model's task-completion gradient, which treats
 * a blocked step as an obstacle and re-attempts it in variant form, probing a
 * bash scanner this core documents as heuristic. Still noise: explaining what
 * the command does, repeating agreement-level NEVERs that AGENTS.md holds
 * resident (output discipline lives there), and enumerating examples or
 * exemptions — classification is the model's own knowledge, and the exempt
 * sets belong to the matcher, not the prose.
 *
 * The table's `unattended` section is policy annotation, not a verdict: evaluate
 * merely transcribes `unattended.allow` onto the winning ask verdict, and an
 * adapter may honor that mark only while its host is unattended — a host state
 * the core neither knows nor owns. The core still never resolves to allow itself.
 * Adapters learn attendance from their host: pi from its session mode and hasUI;
 * the subprocess hooks from AGENTS_UNATTENDED=1 in their environment, read
 * through isUnattended() below (an adapter contract, not a core input).
 *
 * Exemption matrix — how paths escape the scope classes (matches evaluate):
 *   - read intents:  readAllowlist exempts the secret scan, read-scope, and
 *     underPrefixes; the project root and the class's allowPrefixes exempt
 *     read-scope only.
 *   - write intents: writeExemptPrefixes exempt the secret scan,
 *     tree-external-write, and underPrefixes; the project root exempts
 *     tree-external-write only.
 *   - bashScope "external" classes gate on the project root plus the
 *     write-exempt prefixes: the scratch dirs count as interior (per-agent,
 *     wiped at session end — the same footing the workspace holds through
 *     git). A secret inside the project root still asks: the root does not
 *     exempt the secret scan, only the scope classes.
 *   - underPrefixes classes are the pinned-safe doctrine carried to its third
 *     use: like readAllowlist under read-scope and writeExemptPrefixes under
 *     tree-external-write, they keep the scratch dirs exempt from the tmp rule
 *     even if a deployment ever moved XDG_RUNTIME_DIR beneath /tmp.
 *
 * Matching is a discipline backstop, not a security sandbox: bash command scanning
 * is heuristic (two-segment absolute-path tokens, redirection and write-verb
 * targets) and can be evaded by an adversarial command. The tools' native
 * permission systems and sandboxes remain the floor; this core only makes the
 * working agreement's action-shaped rules deterministic.
 *
 * No dependencies beyond node builtins — loadable by every runtime that hosts
 * it: pi's extension host (jiti under node or deno, per install method) and
 * the guard adapters under node. Deno globals are forbidden here for exactly
 * that reason — they would crash the node side.
 *
 * The three standalone adapters run under the host node — the same runtime
 * the agent CLIs themselves need, so the guard adds no dependency of its own
 * (native TS stripping: node ≥ 22.18). No sandbox flags: matching is the
 * discipline backstop described above, and the module reads only its own
 * rules.json plus stat walks. After any change here, feed each adapter a
 * deny, an ask, and a verdictless event under node.
 */

import { existsSync, readFileSync } from "node:fs";

export type Intent =
	| { kind: "bash"; command: string }
	| { kind: "read"; path: string }
	| { kind: "write"; path: string };

export interface Verdict {
	action: "ask" | "deny";
	rule: string;
	reason: string;
	/**
	 * Transcribed from the table's `unattended.allow` entry for this rule — the
	 * user's standing configuration, not a core decision. An adapter may approve
	 * on it only while its host is unattended; attended runs and deny verdicts
	 * never carry it.
	 */
	unattended?: "allow";
}

export interface MatchContext {
	cwd: string;
	home: string;
	/**
	 * The session workspace: the git checkout containing the tool session's
	 * cwd (see workspaceRoot). The scope classes exempt it unconditionally,
	 * independent of the home allowlist. Defaults to cwd.
	 */
	projectRoot?: string;
}

interface SecretNames {
	/** Path segments that always mark a secret location (e.g. ".ssh"). */
	segments?: string[];
	/** Basename patterns: exact, "*.suffix", "prefix*", "prefix*suffix". */
	names?: string[];
}

/**
 * Path predicates a class may combine; the closed set below is what evaluate
 * implements one branch per field. A further predicate shape is the trigger to
 * design combinators, not to add another branch.
 */
interface RuleClass {
	id: string;
	action: "ask" | "deny";
	reason: string;
	/** Regexes tested against the whole command (bash intents only). */
	bash?: string[];
	/** Path predicate: these names mark a secret path (all intents). */
	secretNames?: SecretNames;
	/**
	 * Intent kind the path predicates test. The outside-test reads it as a
	 * plain gate; underPrefixes also accepts "any" (both scanned sets).
	 */
	pathKind?: "read" | "write" | "any";
	/** Path predicate: where the path must sit to match. */
	outside?: "project";
	/**
	 * Bash-class scope: "external" gates the class on the command referencing
	 * at least one path token outside the session workspace or the write-exempt
	 * prefixes (the per-agent scratch dirs — same footing as the workspace).
	 * Tokens are the command's write/read targets, already scanned. Classes
	 * without it apply wherever the text matches. Heuristic: a token-less
	 * command (`rm -rf *`) counts as interior — glob-only targets are opaque to
	 * the scanner in both directions.
	 */
	bashScope?: "external";
	/**
	 * Path predicate: intents of `pathKind` match when a path sits under none of
	 * these roots (`~`-expanded against home). The project root and the read
	 * allowlist are implicitly exempt — pinned-safe trees stay pinned-safe under
	 * any scope rule.
	 */
	allowPrefixes?: string[];
	/**
	 * Path predicate: intents of `pathKind` match when a scanned path sits under
	 * one of these roots (`~`-expanded against home, like allowPrefixes). Unlike
	 * the scope predicates, no class-level exemptions exist — but the pinned-safe
	 * prefixes still win (readAllowlist for read intents, writeExemptPrefixes for
	 * write intents), so the scratch dirs can never be caught by a tmp-style
	 * rule. For "any", each path is tested against its own intent's prefix list.
	 */
	underPrefixes?: string[];
}

/** Standing pre-approvals and the ask timeout for unattended hosts. */
interface UnattendedSection {
	/**
	 * Ask dialogs auto-cancel (hence deny) after this many milliseconds; 0 means
	 * deny without waiting. Non-finite or negative values fail at load.
	 */
	timeoutMs?: number;
	/**
	 * Rule ids an unattended adapter may approve without a user. Every id must
	 * name an existing ask-class rule; anything else fails at load.
	 */
	allow?: string[];
}

interface Rules {
	readAllowlist: string[];
	writeExemptPrefixes: string[];
	unattended?: UnattendedSection;
	classes: (Omit<RuleClass, "bash"> & { bash?: RegExp[] })[];
}

let cached: Rules | undefined;

/**
 * Named prefix anchors substitutable into bash patterns as {{name}}. The git
 * anchor pins the subcommand to the first token after `git` that is not a
 * flag: flag tokens are absorbed one per repetition (with one value apiece,
 * recovered by backtracking), so words in a `commit -m "..."` message body
 * never sit in subcommand position. An unknown anchor fails loudly at load.
 */
const BASH_ANCHORS: Record<string, string> = {
	git: "\\bgit\\s+(?:-\\S+\\s+(?:\\S+\\s+)?)*",
};

function expandAnchors(pattern: string): string {
	return pattern.replace(/\{\{(\w+)\}\}/g, (_match, name: string) => {
		const anchor = BASH_ANCHORS[name];
		if (!anchor) throw new Error(`rules.json: unknown bash anchor {{${name}}}`);
		return anchor;
	});
}

function loadRules(): Rules {
	if (cached) return cached;
	const raw = JSON.parse(
		readFileSync(new URL("../rules.json", import.meta.url), "utf8"),
	) as {
		readAllowlist: string[];
		writeExemptPrefixes: string[];
		unattended?: UnattendedSection;
		classes: RuleClass[];
	};
	// The unattended section gates standing pre-approvals, so malformed data fails
	// loudly here like every other field (bad regexes already throw at compile).
	// The alternative is silent misdirection: a non-array allow (say a bare
	// string) would hit String.prototype.includes in evaluate and mark every ask
	// whose rule id is a substring of it — widening approvals; a typo'd or
	// deny-class id would silently drop the user's intent.
	const classById = new Map(raw.classes.map((c) => [c.id, c] as const));
	if (raw.unattended) {
		const { allow, timeoutMs } = raw.unattended;
		if (allow !== undefined) {
			if (!Array.isArray(allow) || allow.some((id) => typeof id !== "string")) {
				throw new Error("rules.json: unattended.allow must be an array of rule id strings");
			}
			for (const id of allow) {
				const cls = classById.get(id);
				if (!cls) {
					throw new Error(`rules.json: unattended.allow references unknown rule id "${id}"`);
				}
				if (cls.action === "deny") {
					throw new Error(`rules.json: unattended.allow lists "${id}" — deny classes never ask, so they cannot be pre-approved`);
				}
			}
		}
		if (timeoutMs !== undefined && (!Number.isFinite(timeoutMs) || timeoutMs < 0)) {
			throw new Error("rules.json: unattended.timeoutMs must be a non-negative finite number");
		}
	}
	cached = {
		readAllowlist: raw.readAllowlist,
		writeExemptPrefixes: raw.writeExemptPrefixes,
		unattended: raw.unattended,
		classes: raw.classes.map((c) => ({
			...c,
			bash: c.bash?.map((p) => new RegExp(expandAnchors(p))),
		})),
	};
	return cached;
}

// ---------------------------------------------------------------------------
// path helpers
// ---------------------------------------------------------------------------

/** Expand ~, absolutize against cwd, and resolve "." and ".." segments. */
function normalizePath(rawPath: string, ctx: MatchContext): string {
	let p = rawPath.trim().replace(/^["']|["']$/g, "");
	if (p.startsWith("~/")) p = ctx.home + p.slice(1);
	else if (p.startsWith("$HOME/")) p = ctx.home + p.slice(5);
	else if (p === "~") p = ctx.home;
	if (!p.startsWith("/")) p = `${ctx.cwd}/${p}`;
	const out: string[] = [];
	for (const seg of p.split("/")) {
		if (!seg || seg === ".") continue;
		if (seg === "..") out.pop();
		else out.push(seg);
	}
	return `/${out.join("/")}`;
}

function isUnder(path: string, prefix: string): boolean {
	return path === prefix || path.startsWith(`${prefix}/`);
}

/**
 * The session workspace: the git checkout containing `dir` — the nearest
 * ancestor of `dir` (including itself) holding a `.git` entry, whether a
 * directory (ordinary repo, submodule) or a file (linked worktree); `dir`
 * itself when none exists. Deliberately stat-based, no subprocess: the guard
 * judges directory layout, and `$GIT_DIR` overrides are git-session concerns
 * that a directory-level trust model does not follow.
 */
export function workspaceRoot(dir: string): string {
	const segments = dir.split("/").filter(Boolean);
	for (let i = segments.length; i >= 1; i--) {
		const base = `/${segments.slice(0, i).join("/")}`;
		if (hasGitEntry(base)) return base;
	}
	return dir;
}

/**
 * An unreadable ancestor must read as "no git root here", not a crash — the
 * walk then falls through and the workspace degrades to the cwd itself. The
 * failure shape differs by runtime: deno's existsSync throws NotCapable where
 * node's returns false, so the catch carries both.
 */
function hasGitEntry(base: string): boolean {
	try {
		return existsSync(`${base}/.git`);
	} catch {
		return false;
	}
}

/** Rule prefixes may anchor at home: `~` and `~/x` expand against ctx.home. */
function expandPrefix(prefix: string, ctx: MatchContext): string {
	if (prefix === "~") return ctx.home;
	if (prefix.startsWith("~/")) return `${ctx.home}${prefix.slice(1)}`;
	return prefix;
}

/** Glob with one star: exact, "*.suffix", "prefix*", "prefix*suffix". */
function globMatch(pattern: string, s: string): boolean {
	const star = pattern.indexOf("*");
	if (star < 0) return s === pattern;
	const head = pattern.slice(0, star);
	const tail = pattern.slice(star + 1);
	return s.length >= head.length + tail.length && s.startsWith(head) && s.endsWith(tail);
}

// ---------------------------------------------------------------------------
// bash command scanning
// ---------------------------------------------------------------------------

/**
 * Absolute-ish path tokens: `~/x` (any depth), `$HOME/x/y` (two or more
 * segments after the variable — quoted "$HOME/x" keeps the boundary via the
 * quote), and `/x/y` (two or more segments — the depth floor keeps `/regex/`
 * and flag-like tokens out). The leading boundary class excludes word
 * characters so `usr/bin` inside a word never matches.
 */
const BASH_PATH =
	/(?:^|[\s=;&|('",)])(~\/[A-Za-z0-9._-]+(?:\/[A-Za-z0-9._*{}-]+)*|\$HOME(?:\/[A-Za-z0-9._*{}-]+)+|\/[A-Za-z0-9._-]+(?:\/[A-Za-z0-9._*{}-]+)+)/g;

/**
 * Redirect targets — `>` and `>>`, with or without a preceding fd number.
 * The bare-token class excludes parens: a group-closing paren must not glue
 * onto the target (`... 2>/dev/null)` would otherwise normalize to the
 * phantom path "/dev/null)", losing the /dev/null write exemption).
 */
const REDIRECT_TARGET = /(?:^|[\s;(&|])\d?>>?\s*("[^"]*"|'[^']*'|[^\s;&|<>()"]+)/g;

const WRITE_VERB =
	/\b(cp|mv|install|ln|tee|chmod|chown|touch|mkdir|rmdir|rm|truncate|shred|rsync|tar|unzip)\b/;

function bashReadPaths(command: string): string[] {
	return [...command.matchAll(BASH_PATH)].map((m) => m[1]);
}

/** Redirect targets always; with a write verb present, every path token counts. */
function bashWriteTargets(command: string): string[] {
	const targets = [...command.matchAll(REDIRECT_TARGET)].map((m) => m[1]);
	const dd = command.match(/\bdd\b[^;&|]*\bof=([^\s;&|]+)/);
	if (dd) targets.push(dd[1]);
	if (WRITE_VERB.test(command)) targets.push(...bashReadPaths(command));
	return targets;
}

// ---------------------------------------------------------------------------
// evaluation
// ---------------------------------------------------------------------------

function isSecretPath(path: string, sn: SecretNames): boolean {
	const segments = path.split("/");
	if (sn.segments?.some((s) => segments.includes(s))) return true;
	const base = segments[segments.length - 1];
	return sn.names?.some((n) => globMatch(n, base)) ?? false;
}

export function evaluate(intent: Intent, ctx: MatchContext): Verdict | null {
	const rules = loadRules();
	const command = intent.kind === "bash" ? intent.command : "";
	const readPaths: string[] = [];
	const writePaths: string[] = [];
	if (intent.kind === "read") readPaths.push(normalizePath(intent.path, ctx));
	if (intent.kind === "write") writePaths.push(normalizePath(intent.path, ctx));
	if (intent.kind === "bash") {
		// Write targets are excluded from the read set: `> /tmp/foo` writes,
		// it does not read — double-counting one path in both classes would
		// ask twice under two different rule names.
		const targets = bashWriteTargets(command);
		for (const raw of targets) writePaths.push(normalizePath(raw, ctx));
		for (const raw of bashReadPaths(command)) {
			const p = normalizePath(raw, ctx);
			if (!writePaths.includes(p)) readPaths.push(p);
		}
	}
	// Pinned-safe prefixes skip the path classes entirely; rule classes still
	// apply to everything else.
	const secretCandidates = [
		...readPaths.filter((p) => !rules.readAllowlist.some((a) => isUnder(p, a))),
		...writePaths.filter((p) => !rules.writeExemptPrefixes.some((a) => isUnder(p, a))),
	];

	const verdicts: Verdict[] = [];
	for (const cls of rules.classes) {
		let hit = false;
		if (command && cls.bash) {
			if (cls.bashScope === "external") {
				const root = ctx.projectRoot ?? ctx.cwd;
				// Scratch dirs are interior like the workspace: per-agent, wiped at
				// session end, nothing git tracks — deletion there is no more
				// irrecoverable than the in-tree deletion this class already allows.
				// The read allowlist is deliberately absent: pinned-safe READS (say
				// /usr/share) are not thereby deletable.
				const interior = (p: string) =>
					isUnder(p, root) || rules.writeExemptPrefixes.some((a) => isUnder(p, a));
				hit =
					[...writePaths, ...readPaths].some((p) => !interior(p)) &&
					cls.bash.some((re) => re.test(command));
			} else {
				hit = cls.bash.some((re) => re.test(command));
			}
		}
		if (!hit && cls.secretNames) {
			hit = secretCandidates.some((p) => isSecretPath(p, cls.secretNames!));
		}
		if (!hit && cls.allowPrefixes && cls.pathKind === "read") {
			hit = readPaths.some(
				(p) =>
					!isUnder(p, ctx.projectRoot ?? ctx.cwd) &&
					!cls.allowPrefixes!.some((a) => isUnder(p, expandPrefix(a, ctx))) &&
					!rules.readAllowlist.some((a) => isUnder(p, a)),
			);
		}
		if (!hit && cls.underPrefixes) {
			// Each intent kind is exempted by its own pinned-safe prefix list (see
			// the exemption matrix); "any" pools both kinds with their lists.
			const pools: Array<[string[], string[]]> =
				cls.pathKind === "read"
					? [[readPaths, rules.readAllowlist]]
					: cls.pathKind === "write"
					? [[writePaths, rules.writeExemptPrefixes]]
					: [[readPaths, rules.readAllowlist], [writePaths, rules.writeExemptPrefixes]];
			hit = pools.some(([paths, exempt]) =>
				paths.some(
					(p) =>
						!exempt.some((a) => isUnder(p, a)) &&
						cls.underPrefixes!.some((t) => isUnder(p, expandPrefix(t, ctx))),
				),
			);
		}
		if (!hit && cls.outside === "project" && cls.pathKind === "write") {
			const root = ctx.projectRoot ?? ctx.cwd;
			hit = writePaths.some(
				(p) => !isUnder(p, root) && !rules.writeExemptPrefixes.some((a) => isUnder(p, a)),
			);
		}
		if (hit) verdicts.push({ action: cls.action, rule: cls.id, reason: cls.reason });
	}
	// deny outranks ask; within a rank, table order (first hit) wins.
	const deny = verdicts.find((v) => v.action === "deny");
	const winner = deny ?? verdicts[0];
	if (!winner) return null;
	// Transcription only: mark a winning ask whose rule id the table lists in
	// unattended.allow. deny verdicts and unlisted rules pass through unmarked.
	if (winner.action === "ask" && rules.unattended?.allow?.includes(winner.rule)) {
		return { ...winner, unattended: "allow" };
	}
	return winner;
}

/**
 * The cross-tool attendance signal, read fail-safe: AGENTS_UNATTENDED=1 in the
 * environment, exported by the launcher when nobody will answer prompts.
 */
export function isUnattended(): boolean {
	try {
		return process.env.AGENTS_UNATTENDED === "1";
	} catch {
		return false;
	}
}

export interface UnattendedPolicy {
	timeoutMs: number;
	allow: ReadonlySet<string>;
}

/**
 * The table's unattended policy with defaults applied: a 180s ask timeout and an
 * empty allow set when the section or its fields are absent. Malformed table data
 * fails loudly at load, like every other field here.
 */
export function unattendedPolicy(): UnattendedPolicy {
	const rules = loadRules();
	return {
		timeoutMs: rules.unattended?.timeoutMs ?? 180_000,
		allow: new Set(rules.unattended?.allow ?? []),
	};
}
