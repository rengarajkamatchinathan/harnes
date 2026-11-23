// Git graph for the Code project page: parse `git log` output and lay commits out in lanes
// (like VS Code's Git Graph). Pure, for tests.

export interface Commit {
  hash: string;
  parents: string[];
  author: string;
  date: string;
  refs: Ref[];
  subject: string;
  /** Message body (`%b`, an optional last field; "" for records without it). */
  body: string;
}

export interface Ref {
  name: string;
  kind: "head" | "branch" | "remote" | "tag";
}

/** Records are `\x1e`-terminated, fields `\x1f`-separated: hash, parents, author, ISO date, refs, subject[, body]. */
export function parseLog(raw: string): Commit[] {
  return raw
    .split("\x1e")
    .map((r) => r.replace(/^\s+/, ""))
    .filter(Boolean)
    .map((r) => {
      const [hash = "", parents = "", author = "", date = "", refs = "", subject = "", ...body] = r.split("\x1f");
      return { hash: hash.trim(), parents: parents.split(" ").filter(Boolean), author, date, refs: parseRefs(refs), subject: subject.trim(), body: body.join("\x1f").trim() };
    })
    .filter((c) => /^[0-9a-f]{7,64}$/.test(c.hash));
}

/** `%D`: "HEAD -> main, origin/main, tag: v1.0" → badges. */
export function parseRefs(d: string): Ref[] {
  const out: Ref[] = [];
  for (const part of d.split(",").map((s) => s.trim()).filter(Boolean)) {
    if (part.startsWith("HEAD -> ")) out.push({ name: part.slice(8), kind: "head" });
    else if (part === "HEAD") out.push({ name: "HEAD", kind: "head" });
    else if (part.startsWith("tag: ")) out.push({ name: part.slice(5), kind: "tag" });
    else if (part.includes("/") && !part.startsWith("refs/heads/")) out.push({ name: part, kind: "remote" });
    else out.push({ name: part, kind: "branch" });
  }
  // A remote that only mirrors the checked-out branch (origin/main next to main) stays, but after it.
  return out.sort((a, b) => order(a) - order(b));
}
const order = (r: Ref) => ({ head: 0, branch: 1, remote: 2, tag: 3 })[r.kind];

/** A line inside one row, from lane `from` (top or middle) to lane `to` (middle or bottom). */
export interface Seg {
  from: number;
  to: number;
  /** Lane whose colour the line takes. */
  color: number;
}

export interface Row {
  commit: Commit;
  /** The commit's lane. */
  lane: number;
  /** Lines in the top half (row top → dot height) and bottom half (dot height → row bottom). */
  top: Seg[];
  bottom: Seg[];
  /** Lanes in use on this row (graph width). */
  width: number;
}

/**
 * Lane layout: each lane waits for a commit hash. A commit takes the lane waiting for it (or a
 * free one); other lanes waiting for it converge into it; its first parent continues its lane
 * and further parents (merges) join an existing lane or open a new one.
 */
export function layout(commits: Commit[]): Row[] {
  const lanes: (string | null)[] = [];
  const colors: number[] = [];
  let nextColor = 0;
  const free = () => {
    const i = lanes.indexOf(null);
    return i >= 0 ? i : lanes.push(null) - 1;
  };
  const rows: Row[] = [];
  const known = new Set(commits.map((c) => c.hash));

  for (const c of commits) {
    const before = [...lanes];
    let lane = lanes.indexOf(c.hash);
    if (lane < 0) {
      lane = free();
      colors[lane] = nextColor++;
    }
    const top: Seg[] = [];
    before.forEach((h, i) => {
      if (h == null) return;
      if (h === c.hash) top.push({ from: i, to: lane, color: colors[i] });
      else top.push({ from: i, to: i, color: colors[i] });
    });
    // Lanes that converged into this commit are free from here on.
    before.forEach((h, i) => {
      if (h === c.hash && i !== lane) lanes[i] = null;
    });

    const bottom: Seg[] = [];
    const parents = c.parents.filter((p) => known.has(p) || c.parents.length === 1);
    lanes[lane] = parents[0] ?? null;
    if (lanes[lane]) bottom.push({ from: lane, to: lane, color: colors[lane] });
    for (const p of parents.slice(1)) {
      let j = lanes.indexOf(p);
      if (j < 0) {
        j = free();
        lanes[j] = p;
        colors[j] = nextColor++;
      }
      bottom.push({ from: lane, to: j, color: colors[j] });
    }
    // Lanes passing through untouched.
    lanes.forEach((h, i) => {
      if (h != null && i !== lane && before[i] === h && !bottom.some((s) => s.to === i && s.from === lane)) bottom.push({ from: i, to: i, color: colors[i] });
    });
    while (lanes.length && lanes[lanes.length - 1] == null) lanes.pop();
    const width = Math.max(lane + 1, before.length, lanes.length, ...bottom.map((s) => s.to + 1));
    rows.push({ commit: c, lane, top, bottom, width });
  }
  return rows;
}

/** Relative date like VS Code's Git Graph ("5 mins ago", "3 days ago", then the date). */
export function ago(iso: string, now = Date.now()): string {
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "";
  const s = Math.max(0, Math.round((now - t) / 1000));
  if (s < 60) return "just now";
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min${m === 1 ? "" : "s"} ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h} hour${h === 1 ? "" : "s"} ago`;
  const d = Math.round(h / 24);
  if (d < 30) return `${d} day${d === 1 ? "" : "s"} ago`;
  return new Date(t).toLocaleDateString();
}
