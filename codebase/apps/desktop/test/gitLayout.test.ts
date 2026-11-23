import { describe, expect, it } from "vitest";
import { layout, parseLog, parseRefs } from "../src/ui/code/gitLayout";

const rec = (hash: string, parents: string[], refs = "", subject = "s") => [hash, parents.join(" "), "Sam", "2026-01-01T10:00:00Z", refs, subject].join("\x1f") + "\x1e";
const H = (n: number) => String(n).repeat(7);

describe("git graph", () => {
  it("parses log records and ref badges", () => {
    const cs = parseLog(rec(H(2), [H(1)], "HEAD -> main, origin/main, tag: v1") + "\n" + rec(H(1), []));
    expect(cs.map((c) => c.hash)).toEqual([H(2), H(1)]);
    expect(cs[0].refs).toEqual([
      { name: "main", kind: "head" },
      { name: "origin/main", kind: "remote" },
      { name: "v1", kind: "tag" },
    ]);
    expect(parseRefs("feature/x")).toEqual([{ name: "feature/x", kind: "remote" }]);
  });

  it("keeps a straight history in one lane", () => {
    const rows = layout(parseLog(rec(H(3), [H(2)]) + rec(H(2), [H(1)]) + rec(H(1), [])));
    expect(rows.map((r) => r.lane)).toEqual([0, 0, 0]);
    expect(rows[2].bottom).toEqual([]);
  });

  it("opens a lane for a merged branch and closes it where it forks", () => {
    // 4 = merge(3, 2); 3 and 2 both come from 1.
    const rows = layout(parseLog(rec(H(4), [H(3), H(2)]) + rec(H(3), [H(1)]) + rec(H(2), [H(1)]) + rec(H(1), [])));
    expect(rows.map((r) => r.lane)).toEqual([0, 0, 1, 0]);
    expect(rows[0].bottom).toContainEqual(expect.objectContaining({ from: 0, to: 1 }));
    // Commit 1: the branch lane converges back into lane 0.
    expect(rows[3].top).toContainEqual(expect.objectContaining({ from: 1, to: 0 }));
    expect(Math.max(...rows.map((r) => r.width))).toBe(2);
  });
});
