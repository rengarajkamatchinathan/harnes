import { describe, expect, it } from "vitest";
import { branchOnly, countBySession, linkCommits, trailerSession, withTrailer } from "../src/ui/code/commitLinks";
import { parseLog, type Commit } from "../src/ui/code/gitLayout";

const H = (n: number) => String(n).repeat(7);
const commit = (n: number, parents: number[], date: string, opts: { body?: string; refs?: Commit["refs"] } = {}): Commit => ({
  hash: H(n),
  parents: parents.map(H),
  author: "Sam",
  date,
  refs: opts.refs ?? [],
  subject: `c${n}`,
  body: opts.body ?? "",
});
const T = (iso: string) => Date.parse(iso);

describe("commit log body field", () => {
  const base = [H(1), "", "Sam", "2026-01-01T10:00:00Z", "", "Subject"];
  it("parses old 6-field records with an empty body", () => {
    const [c] = parseLog(base.join("\x1f") + "\x1e");
    expect(c.subject).toBe("Subject");
    expect(c.body).toBe("");
  });
  it("parses the trailing body field", () => {
    const [c] = parseLog([...base, "Details\n\nGenie-Session: ses_1\n"].join("\x1f") + "\x1e\n");
    expect(c.subject).toBe("Subject");
    expect(c.body).toBe("Details\n\nGenie-Session: ses_1");
  });
});

describe("trailers", () => {
  it("reads the last Genie-Session trailer", () => {
    expect(trailerSession("x\n\nGenie-Session: ses_a")).toBe("ses_a");
    expect(trailerSession("genie-session:  ses_b  \nGenie-Session: ses_c\n")).toBe("ses_c");
    expect(trailerSession("mentions Genie-Session: inline only")).toBeNull();
  });
  it("appends the trailer once and leaves empty messages alone", () => {
    expect(withTrailer("Fix bug\n", "ses_1")).toBe("Fix bug\n\nGenie-Session: ses_1");
    expect(withTrailer(withTrailer("Fix bug", "ses_1"), "ses_1")).toBe("Fix bug\n\nGenie-Session: ses_1");
    expect(withTrailer("  ", "ses_1")).toBe("  ");
  });
});

describe("linkCommits", () => {
  const s1 = { id: "s1", start: T("2026-01-01T10:00:00Z"), end: T("2026-01-01T11:00:00Z") };
  const s2 = { id: "s2", start: T("2026-01-01T10:30:00Z"), end: T("2026-01-01T12:00:00Z") };

  it("links an exact trailer even when the session is unknown or the time says otherwise", () => {
    const cs = [commit(1, [], "2026-01-05T00:00:00Z", { body: "Genie-Session: gone" }), commit(2, [1], "2026-01-01T10:10:00Z", { body: "Genie-Session: s2" })];
    const links = linkCommits(cs, [s1]);
    expect(links.get(H(1))).toEqual({ sessionId: "gone", how: "trailer" });
    expect(links.get(H(2))).toEqual({ sessionId: "s2", how: "trailer" });
  });

  it("links by time when exactly one session's window (with grace) matches", () => {
    const cs = [commit(1, [], "2026-01-01T10:05:00Z"), commit(2, [1], "2026-01-01T11:08:00Z"), commit(3, [2], "2026-01-01T11:30:00Z")];
    const links = linkCommits(cs, [s1]);
    expect(links.get(H(1))).toEqual({ sessionId: "s1", how: "likely" });
    expect(links.get(H(2))).toEqual({ sessionId: "s1", how: "likely" }); // within the 10 min grace
    expect(links.has(H(3))).toBe(false);
  });

  it("leaves ambiguous commits (overlapping sessions) unlinked", () => {
    const cs = [commit(1, [], "2026-01-01T10:45:00Z"), commit(2, [1], "2026-01-01T11:45:00Z")];
    const links = linkCommits(cs, [s1, s2]);
    expect(links.has(H(1))).toBe(false);
    expect(links.get(H(2))).toEqual({ sessionId: "s2", how: "likely" });
  });

  it("links commits only reachable from a session's worktree branch", () => {
    // 1 ← 2 (main) ; 1 ← 3 ← 4 (genie/fix)
    const cs = [
      commit(4, [3], "2026-02-01T00:00:00Z", { refs: [{ name: "genie/fix", kind: "remote" }] }),
      commit(3, [1], "2026-02-01T00:00:00Z"),
      commit(2, [1], "2026-02-01T00:00:00Z", { refs: [{ name: "main", kind: "head" }] }),
      commit(1, [], "2026-02-01T00:00:00Z"),
    ];
    expect([...branchOnly(cs, "genie/fix")].sort()).toEqual([H(3), H(4)]);
    const links = linkCommits(cs, [{ id: "w", start: NaN, end: NaN, branch: "genie/fix" }]);
    expect(links.get(H(4))?.how).toBe("branch");
    expect(links.has(H(2))).toBe(false);
    expect(countBySession(links)).toEqual(new Map([["w", 2]]));
  });
});
