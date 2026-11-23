// The project's git history as a graph, like VS Code's Git Graph: coloured lanes, branch/tag
// badges, "Uncommitted changes" on top; click a commit for its details.
import { Fragment, useEffect, useMemo, useState } from "react";
import { Icon, IconButton } from "@genie/ui";
import { t } from "../../i18n";
import { useApp } from "../context";
import { GenieMark } from "../GenieMark";
import type { CommitLink } from "./commitLinks";
import { ago, layout, parseLog, type Commit, type Row, type Seg } from "./gitLayout";

export interface GitGraphLinks {
  /** Commit hash → the session it was made in. */
  links: Map<string, CommitLink>;
  /** Title for a linked session; null when it no longer exists. */
  title: (sessionId: string) => string | null;
  /** Show only this session's commits at full strength (others dimmed). */
  filter: string | null;
  onClearFilter: () => void;
}

const ROW = 28;
const LANE = 14;
const PAD = 8;
const DOT = 4;
const COLORS = ["#0085d9", "#d9008f", "#00c853", "#d98500", "#a300d9", "#e53935", "#00bfa5", "#e138e8"];
const color = (n: number) => COLORS[n % COLORS.length];
const x = (lane: number) => PAD + lane * LANE + LANE / 2;
const MAX_LANES = 12;
/** Room for the "Graph" header even when the history is a single lane. */
const GRAPH_MIN = 64;

function path(s: Seg, y0: number, y1: number): string {
  const x0 = x(s.from);
  const x1 = x(s.to);
  if (x0 === x1) return `M ${x0} ${y0} L ${x1} ${y1}`;
  const k = (y1 - y0) * 0.8;
  return `M ${x0} ${y0} C ${x0} ${y0 + k}, ${x1} ${y1 - k}, ${x1} ${y1}`;
}

function GraphCell({ row, w, head, linkTop }: { row: Row; w: number; head: boolean; linkTop: boolean }) {
  const mid = ROW / 2;
  const c = color(row.top.find((s) => s.to === row.lane)?.color ?? row.bottom.find((s) => s.from === row.lane)?.color ?? row.lane);
  return (
    <svg className="gitgraph__svg" width={w} height={ROW} aria-hidden>
      {linkTop && <path d={`M ${x(row.lane)} 0 L ${x(row.lane)} ${mid}`} stroke={c} strokeWidth={2} strokeDasharray="3 3" fill="none" />}
      {row.top.map((s, i) => (
        <path key={`t${i}`} d={path(s, 0, mid)} stroke={color(s.color)} strokeWidth={2} fill="none" />
      ))}
      {row.bottom.map((s, i) => (
        <path key={`b${i}`} d={path(s, mid, ROW)} stroke={color(s.color)} strokeWidth={2} fill="none" />
      ))}
      <circle cx={x(row.lane)} cy={mid} r={head ? DOT + 0.5 : DOT} fill={head ? "var(--surface)" : c} stroke={c} strokeWidth={head ? 2 : 0} />
    </svg>
  );
}

export function GitGraph({ path: folder, onCommits, linked }: { path: string; onCommits?: (commits: Commit[]) => void; linked?: GitGraphLinks }) {
  const app = useApp();
  const [data, setData] = useState<{ log: string; status: string; branch: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [shown, setShown] = useState(60);
  const [open, setOpen] = useState<string | null>(null);

  const load = () => {
    setLoading(true);
    app.platform.gitGraph(folder, 400).then(
      (d) => {
        setData(d);
        setError(null);
        setLoading(false);
      },
      (e) => {
        setError(String((e as Error)?.message ?? e));
        setLoading(false);
      },
    );
  };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(load, [folder]);

  const commits = useMemo(() => (data ? parseLog(data.log) : []), [data]);
  const rows = useMemo(() => layout(commits), [commits]);
  useEffect(() => onCommits?.(commits), [commits, onCommits]);
  const filter = linked?.filter ?? null;
  const changes = data?.status.split("\n").filter((l) => l.trim()).length ?? 0;
  const visible = rows.slice(0, shown);
  const w = PAD * 2 + Math.min(MAX_LANES, Math.max(1, ...visible.map((r) => r.width))) * LANE;
  const headIdx = rows.findIndex((r) => r.commit.refs.some((f) => f.kind === "head"));
  const headLane = headIdx >= 0 ? rows[headIdx].lane : 0;

  const notRepo = !!error && /not a git repository/i.test(error);
  return (
    <section className="gitgraph">
      <div className="gitgraph__head">
        <h3 className="section-title">{t("git.graph")}</h3>
        {data?.branch && (
          <span className="gitgraph__branch">
            <Icon name="branch" size={12} /> {data.branch}
          </span>
        )}
        <span className="grow" />
        <IconButton icon="refresh" size="sm" label={t("git.refresh")} onClick={load} disabled={loading} />
      </div>

      {loading && !data && <p className="muted settings-note">{t("common.loading")}</p>}
      {error && <p className="muted settings-note">{notRepo ? t("git.notRepo") : t("git.failed", { error })}</p>}
      {data && rows.length === 0 && !error && <p className="muted settings-note">{t("git.noCommits")}</p>}

      {filter && linked && (
        <div className="gitgraph__filter">
          <span className="gitgraph__chip">
            <GenieMark size={12} />
            <span className="gitgraph__ellip">{t("git.commitsFrom", { title: linked.title(filter) ?? t("git.deletedSession") })}</span>
            <IconButton icon="x" size="sm" label={t("git.clearFilter")} onClick={linked.onClearFilter} />
          </span>
        </div>
      )}

      {rows.length > 0 && (
        <div className="gitgraph__table" role="table" aria-label={t("git.graph")} style={{ ["--gg-graph" as string]: `${Math.max(w, GRAPH_MIN)}px` }}>
          <div className="gitgraph__row gitgraph__row--header" role="row">
            <span role="columnheader">{t("git.col.graph")}</span>
            <span role="columnheader">{t("git.col.description")}</span>
            <span role="columnheader">{t("git.col.date")}</span>
            <span role="columnheader">{t("git.col.author")}</span>
            <span role="columnheader">{t("git.col.commit")}</span>
          </div>

          {changes > 0 && (
            <div className="gitgraph__row gitgraph__row--wip" role="row">
              <svg className="gitgraph__svg" width={w} height={ROW} aria-hidden>
                {headIdx === 0 && <path d={`M ${x(headLane)} ${ROW / 2} L ${x(headLane)} ${ROW}`} stroke="var(--text-muted)" strokeWidth={2} strokeDasharray="3 3" fill="none" />}
                <circle cx={x(headLane)} cy={ROW / 2} r={DOT} fill="var(--surface)" stroke="var(--text-muted)" strokeWidth={2} />
              </svg>
              <span className="gitgraph__desc">
                <em>{t("git.uncommitted", { n: changes })}</em>
              </span>
              <span className="gitgraph__muted">*</span>
              <span className="gitgraph__muted">*</span>
              <span className="gitgraph__muted">*</span>
            </div>
          )}

          {visible.map((r, i) => {
            const c = r.commit;
            const isOpen = open === c.hash;
            const link = linked?.links.get(c.hash);
            const dim = !!filter && link?.sessionId !== filter;
            const linkTitle = link ? (linked!.title(link.sessionId) ?? null) : null;
            return (
              <Fragment key={c.hash}>
                <button
                  type="button"
                  className={`gitgraph__row ${isOpen ? "is-open" : ""} ${dim ? "is-dim" : ""}`}
                  role="row"
                  aria-expanded={isOpen}
                  onClick={() => setOpen(isOpen ? null : c.hash)}
                >
                  <GraphCell row={r} w={w} head={i === headIdx} linkTop={i === 0 && headIdx === 0 && changes > 0} />
                  <span className="gitgraph__desc">
                    {c.refs.map((f) => (
                      <span key={f.kind + f.name} className={`gitref gitref--${f.kind}`} style={f.kind === "head" || f.kind === "branch" ? { borderColor: color(r.top.find((s) => s.to === r.lane)?.color ?? r.lane) } : undefined}>
                        {f.kind === "tag" ? <Icon name="pin" size={10} /> : <Icon name="branch" size={10} />}
                        {f.name}
                      </span>
                    ))}
                    {link && (
                      <span className={`gitgraph__genie ${link.how === "likely" ? "is-likely" : ""}`} title={link.how === "likely" ? t("git.linkedLikelyTip") : t("git.linkedTip")}>
                        <GenieMark size={12} state="still" />
                      </span>
                    )}
                    <span className="gitgraph__subject">{c.subject}</span>
                  </span>
                  <span className="gitgraph__muted" title={new Date(c.date).toLocaleString()}>
                    {ago(c.date)}
                  </span>
                  <span className="gitgraph__muted gitgraph__ellip">{c.author}</span>
                  <span className="gitgraph__hash">{c.hash.slice(0, 7)}</span>
                </button>
                {isOpen && (
                  <div className="gitgraph__detail">
                    <dl>
                      <div>
                        <dt>{t("git.col.commit")}</dt>
                        <dd className="gitgraph__mono">
                          {c.hash}
                          <IconButton icon="copy" size="sm" label={t("chat.copy")} onClick={() => void navigator.clipboard?.writeText(c.hash)} />
                        </dd>
                      </div>
                      {c.parents.length > 0 && (
                        <div>
                          <dt>{c.parents.length > 1 ? t("git.parents") : t("git.parent")}</dt>
                          <dd className="gitgraph__mono">{c.parents.map((p) => p.slice(0, 7)).join(", ")}</dd>
                        </div>
                      )}
                      <div>
                        <dt>{t("git.col.author")}</dt>
                        <dd>{c.author}</dd>
                      </div>
                      <div>
                        <dt>{t("git.col.date")}</dt>
                        <dd>{new Date(c.date).toLocaleString()}</dd>
                      </div>
                      {link && (
                        <div>
                          <dt>{t("git.madeIn")}</dt>
                          <dd>
                            {linkTitle != null ? (
                              <button type="button" className="gitgraph__session" onClick={() => app.selectSession(link.sessionId)}>
                                <GenieMark size={12} state="still" />
                                {linkTitle}
                              </button>
                            ) : (
                              <span className="muted">{t("git.deletedSession")}</span>
                            )}
                            {link.how === "likely" && <span className="muted"> {t("git.likely")}</span>}
                          </dd>
                        </div>
                      )}
                    </dl>
                    <p className="gitgraph__message">{c.subject}</p>
                    {c.body && <pre className="gitgraph__body">{c.body}</pre>}
                  </div>
                )}
              </Fragment>
            );
          })}
        </div>
      )}
      {rows.length > shown && (
        <button type="button" className="gitgraph__more" onClick={() => setShown((n) => n + 100)}>
          {t("git.showMore", { n: Math.min(100, rows.length - shown) })}
        </button>
      )}
    </section>
  );
}
