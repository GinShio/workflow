/**
 * hooks/core — the portable matcher behind every tool's guard adapter.
 *
 * One rule table (../rules.json), one verdict model: `deny` beats `ask`, and no
 * match returns null. The core never emits `allow` — allowing is the absence of a
 * verdict, so a guard that fails to match can never waive a rule.
 *
 * Matching is a discipline backstop, not a security sandbox: bash command scanning
 * is heuristic (two-segment absolute-path tokens, redirection and write-verb
 * targets) and can be evaded by an adversarial command. The tools' native
 * permission systems and sandboxes remain the floor; this core only makes the
 * working agreement's action-shaped rules deterministic.
 *
 * No dependencies beyond node builtins — parseable by any deno process (pi
 * extension or hook command).
 *
 * The three standalone adapters run under sandboxed `deno run` flags carried
 * in each tool's hook config (read: their own hooks dir plus $HOME; sys:
 * homedir). Every deno API this module or an adapter touches must be covered
 * by those flags — test/guard-acceptance.sh runs the adapters under the
 * production flags and fails loudly otherwise; run it after any change here.
 */

import { existsSync } from "node:fs";

export type Intent =
	| { kind: "bash"; command: string }
	| { kind: "read"; path: string }
	| { kind: "write"; path: string };

export interface Verdict {
	action: "ask" | "deny";
	rule: string;
	reason: string;
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

interface RuleClass {
	id: string;
	action: "ask" | "deny";
	reason: string;
	/** Regexes tested against the whole command (bash intents only). */
	bash?: string[];
	/** Path predicate: these names mark a secret path (all intents). */
	secretNames?: SecretNames;
	/** Path predicate: intent kind the outside-test applies to. */
	pathKind?: "read" | "write";
	/** Path predicate: where the path must sit to match. */
	outside?: "project";
	/**
	 * Bash-class scope: "external" gates the class on the command referencing
	 * at least one path token outside the session workspace (its write/read
	 * target tokens, already scanned). Classes without it apply wherever the
	 * text matches. Heuristic: a token-less command (`rm -rf *`) counts as
	 * interior — glob-only targets are opaque to the scanner in both
	 * directions.
	 */
	bashScope?: "external";
	/**
	 * Path predicate: intents of `pathKind` match when a path sits under none of
	 * these roots (`~`-expanded against home). The project root and the read
	 * allowlist are implicitly exempt — pinned-safe trees stay pinned-safe under
	 * any scope rule.
	 */
	allowPrefixes?: string[];
}

interface Rules {
	readAllowlist: string[];
	writeExemptPrefixes: string[];
	classes: (Omit<RuleClass, "bash"> & { bash?: RegExp[] })[];
}

let cached: Rules | undefined;

function loadRules(): Rules {
	if (cached) return cached;
	const raw = JSON.parse(
		Deno.readTextFileSync(new URL("../rules.json", import.meta.url)),
	) as {
		readAllowlist: string[];
		writeExemptPrefixes: string[];
		classes: RuleClass[];
	};
	cached = {
		readAllowlist: raw.readAllowlist,
		writeExemptPrefixes: raw.writeExemptPrefixes,
		classes: raw.classes.map((c) => ({
			...c,
			bash: c.bash?.map((p) => new RegExp(p)),
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
 * Sandboxed hook adapters (deno `--allow-read` limited to their own hooks dir
 * plus $HOME) cannot stat ancestors outside that grant: deno's existsSync
 * throws NotCapable instead of returning false. An unreadable ancestor is
 * "no git root here", not a crash — the walk then falls through and the
 * workspace degrades to the cwd itself.
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
 * Absolute-ish path tokens: `~/x` (any depth) and `/x/y` (two or more segments —
 * the depth floor keeps `/regex/` and flag-like tokens out). The leading boundary
 * class excludes word characters so `usr/bin` inside a word never matches.
 */
const BASH_PATH =
	/(?:^|[\s=;&|('",)])(~\/[A-Za-z0-9._-]+(?:\/[A-Za-z0-9._*{}-]+)*|\/[A-Za-z0-9._-]+(?:\/[A-Za-z0-9._*{}-]+)+)/g;

/**
 * Redirect targets — `>` and `>>`, with or without a preceding fd number.
 * The bare-token class excludes parens: a group-closing paren must not glue
 * onto the target (`... 2>/dev/null)` would otherwise normalize to the
 * phantom path "/dev/null)", losing the /dev/null write exemption).
 */
const REDIRECT_TARGET = /(?:^|[\s;(&|])\d?>>?\s*("[^"]*"|'[^']*'|[^\s;&|<>()"]+)/g;

const WRITE_VERB =
	/\b(cp|mv|install|ln|tee|chmod|chown|touch|mkdir|rmdir|rm|truncate|shred)\b/;

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
				hit =
					[...writePaths, ...readPaths].some((p) => !isUnder(p, root)) &&
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
	return deny ?? verdicts[0] ?? null;
}
