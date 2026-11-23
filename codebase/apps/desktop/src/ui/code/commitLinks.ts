// Session ↔ commit links for the Code project page's git graph. Pure, for tests.
//
// Signals, strongest first:
//  1. `trailer`: the commit message carries `Genie-Session: <id>` (Genie's own commits add it).
//  2. `branch`: the commit is reachable only from a session's worktree branch (`genie/<slug>`).
//  3. `likely`: the commit's author time falls inside exactly one session's active window.
import type { Commit } from "./gitLayout";

export const TRAILER = "Genie-Session";
/** Slack around a session's active window for the time heuristic. */
export const GRACE_MS = 10 * 60_000;

export type LinkHow = "trailer" | "branch" | "likely";
export interface CommitLink {
  sessionId: string;
  how: LinkHow;
}

/** A session's active window (first → last message, epoch ms) and its worktree branch, if known. */
export interface SessionWindow {
  id: string;
  start: number;
  end: number;
  branch?: string | null;
}

/** The session id from a `Genie-Session: <id>` trailer in a commit body (the last one wins). */
export function trailerSession(body: string): string | null {
  let id: string | null = null;
  for (const m of body.matchAll(/^genie-session:[ \t]*(\S+)[ \t]*$/gim)) id = m[1];
  return id;
}

/** Append the trailer to a commit message (once). */
export function withTrailer(message: string, sessionId: string): string {
  if (!message.trim() || trailerSession(message) === sessionId) return message;
  return `${message.replace(/\s+$/, "")}\n\n${TRAILER}: ${sessionId}`;
}

/** Commits reachable from `branch`'s tip but from no other branch/remote tip in the log. */
export function branchOnly(commits: Commit[], branch: string): Set<string> {
  const byHash = new Map(commits.map((c) => [c.hash, c]));
  const isOwn = (name: string) => name === branch || name.endsWith(`/${branch}`); // origin/genie/x mirrors it
  const reach = (tips: string[]) => {
    const seen = new Set<string>();
    const stack = [...tips];
    while (stack.length) {
      const h = stack.pop()!;
      if (seen.has(h)) continue;
      seen.add(h);
      for (const p of byHash.get(h)?.parents ?? []) if (byHash.has(p)) stack.push(p);
    }
    return seen;
  };
  const own: string[] = [];
  const other: string[] = [];
  for (const c of commits)
    for (const r of c.refs) {
      if (r.kind === "tag" || r.name === "HEAD") continue;
      (isOwn(r.name) ? own : other).push(c.hash);
    }
  if (!own.length) return new Set();
  const others = reach(other);
  return new Set([...reach(own)].filter((h) => !others.has(h)));
}

/** Link each commit to at most one session: trailer, then worktree branch, then the time heuristic. */
export function linkCommits(commits: Commit[], sessions: SessionWindow[], grace = GRACE_MS): Map<string, CommitLink> {
  const out = new Map<string, CommitLink>();
  for (const c of commits) {
    const id = trailerSession(c.body);
    if (id) out.set(c.hash, { sessionId: id, how: "trailer" });
  }
  for (const s of sessions) {
    if (!s.branch) continue;
    for (const h of branchOnly(commits, s.branch)) if (!out.has(h)) out.set(h, { sessionId: s.id, how: "branch" });
  }
  for (const c of commits) {
    if (out.has(c.hash)) continue;
    const t = Date.parse(c.date);
    if (!Number.isFinite(t)) continue;
    const hits = sessions.filter((s) => Number.isFinite(s.start) && Number.isFinite(s.end) && t >= s.start - grace && t <= s.end + grace);
    if (hits.length === 1) out.set(c.hash, { sessionId: hits[0].id, how: "likely" });
  }
  return out;
}

/** Linked commits per session. */
export function countBySession(links: Map<string, CommitLink>): Map<string, number> {
  const n = new Map<string, number>();
  for (const l of links.values()) n.set(l.sessionId, (n.get(l.sessionId) ?? 0) + 1);
  return n;
}
