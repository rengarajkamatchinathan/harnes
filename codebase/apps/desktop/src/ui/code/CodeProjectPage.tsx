// Code project page: one folder's sessions, like the Chat project page (name, "New session",
// sessions list, details card). Opened from a project row in the Code sidebar.
import { useCallback, useEffect, useMemo, useState } from "react";
import type { SessionSummary } from "@genie/protocol";
import { Button, Dialog, Icon, IconButton, Menu } from "@genie/ui";
import { t } from "../../i18n";
import { sameWorkspace } from "../../state/app";
import { useApp } from "../context";
import { folderName, TitlebarControls } from "../Sidebar";
import { useShell } from "../shell";
import { GenieMark } from "../GenieMark";
import { GitGraph } from "./GitGraph";
import { countBySession, linkCommits, type SessionWindow } from "./commitLinks";
import type { Commit } from "./gitLayout";

const RUNNING = new Set(["running", "awaiting_ask", "paused"]);
/** Sessions whose windows/branches are fetched for commit links (most recent first). */
const MAX_LINKED = 20;

/**
 * Each session's active window (first → last message) and worktree branch (`genie/…` from
 * `git.status`), for linking commits. Refetched (debounced) when the sessions change.
 */
function useSessionWindows(sessions: SessionSummary[]): SessionWindow[] {
  const app = useApp();
  const [windows, setWindows] = useState<SessionWindow[]>([]);
  const list = sessions.slice(0, MAX_LINKED);
  const key = list.map((s) => `${s.session_id}@${s.updated_at}`).join(",");
  useEffect(() => {
    let live = true;
    const timer = setTimeout(() => {
      void Promise.all(
        list.map(async (s): Promise<SessionWindow | null> => {
          const [got, status] = await Promise.all([
            app.client.session.get({ session_id: s.session_id }).catch(() => null),
            app.client.git.status({ session_id: s.session_id }).catch(() => null),
          ]);
          const times = (got?.path ?? []).map((m) => Date.parse(m.created_at)).filter(Number.isFinite);
          const start = times.length ? Math.min(...times) : Date.parse(got?.session.created_at ?? "");
          const end = Math.max(times.length ? Math.max(...times) : -Infinity, RUNNING.has(s.status) ? Date.now() : -Infinity);
          const branch = status?.branch?.startsWith("genie/") ? status.branch : null;
          if (!Number.isFinite(start) && !branch) return null;
          return { id: s.session_id, start, end: Number.isFinite(end) ? end : start, branch };
        }),
      ).then((w) => live && setWindows(w.filter((x): x is SessionWindow => !!x)));
    }, 800);
    return () => {
      live = false;
      clearTimeout(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  return windows;
}

export function CodeProjectPage({ path }: { path: string }) {
  const app = useApp();
  const shell = useShell();
  const [confirm, setConfirm] = useState<string | null>(null);
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });
  const name = folderName(path) || t("code.noProject");
  const sessions = [...app.sessions.values()]
    .filter((s) => s.kind === "code" && !s.archived && (path ? sameWorkspace(s.workspace_id, path) : !s.workspace_id))
    .sort((a, b) => b.updated_at.localeCompare(a.updated_at));
  const newSession = () => app.setUi({ view: "main", codeWorkspace: path, activeSession: null, activeProject: null });
  const last = sessions[0]?.updated_at;
  const display = path.replace(/^\\\\\?\\/, "");

  // Session ↔ commit links for the git graph.
  const [commits, setCommits] = useState<Commit[]>([]);
  const [filter, setFilter] = useState<string | null>(null);
  const windows = useSessionWindows(path ? sessions : []);
  const links = useMemo(() => linkCommits(commits, windows), [commits, windows]);
  const counts = useMemo(() => countBySession(links), [links]);
  const onCommits = useCallback((c: Commit[]) => setCommits(c), []);
  useEffect(() => setFilter(null), [path]);
  const title = (id: string) => {
    const s = app.sessions.get(id);
    return s ? s.title || t("sidebar.untitledSession") : null;
  };

  return (
    <div className="chat">
      <header className={`chat__header ${shell.sidebarHidden ? "chat__header--collapsed" : ""}`} data-tauri-drag-region>
        {shell.sidebarHidden && <TitlebarControls />}
        <span className="chat__title chat__title--muted">{t("code.projects")}</span>
      </header>
      <div className="page">
        <div className="page__inner project">
          <div className="project__head">
            <span className="project__icon" aria-hidden>
              <Icon name="folder" size={18} />
            </span>
            <h1 className="project__name codeproj-page__name" title={display}>
              {name}
            </h1>
          </div>
          {path && <p className="muted codeproj-page__path">{display}</p>}

          <button className="project__new" onClick={newSession}>
            <Icon name="plus" />
            <span>{t("code.newSessionIn", { name })}</span>
          </button>

          <div className="project__grid">
            <section className="project__main">
              <h3 className="section-title">{t("code.sessions")}</h3>
              {sessions.length === 0 ? (
                <p className="muted settings-note">{t("code.noSessions")}</p>
              ) : (
                <div className="list-box">
                  {sessions.map((s) => (
                    <div key={s.session_id} className="list-box__row project__chat">
                      <button className="project__chat-link" onClick={() => app.selectSession(s.session_id)}>
                        {RUNNING.has(s.status) ? (
                          <span className={`dot ${s.status === "running" ? "dot--running" : "dot--ask"}`} aria-label={s.status} />
                        ) : (
                          <span className="sidebar__mark" aria-hidden>
                            <GenieMark size={14} />
                          </span>
                        )}
                        <span className="sidebar__title">{s.title || t("sidebar.untitledSession")}</span>
                        <span className="muted project__when">{new Date(s.updated_at).toLocaleDateString()}</span>
                      </button>
                      {(counts.get(s.session_id) ?? 0) > 0 && (
                        <button
                          type="button"
                          className={`codeproj-page__commits ${filter === s.session_id ? "is-active" : ""}`}
                          title={t("code.showCommits")}
                          aria-pressed={filter === s.session_id}
                          onClick={() => setFilter(filter === s.session_id ? null : s.session_id)}
                        >
                          {counts.get(s.session_id) === 1 ? t("code.commitCountOne") : t("code.commitCount", { n: counts.get(s.session_id)! })}
                        </button>
                      )}
                      <Menu
                        label={t("row.actions")}
                        align="right"
                        items={[
                          { id: "archive", label: t("row.archive"), icon: "folder", onSelect: () => void app.client.session.update({ session_id: s.session_id, archived: true }).catch(fail) },
                          { id: "delete", label: t("row.delete"), icon: "trash", danger: true, separatorBefore: true, keepFocus: true, onSelect: () => setConfirm(s.session_id) },
                        ]}
                        trigger={(tp) => <IconButton icon="more" size="sm" label={t("row.actions")} {...tp} />}
                      />
                    </div>
                  ))}
                </div>
              )}
            </section>

            <aside className="project__side">
              <div className="project__card">
                <div className="project__card-head">
                  <strong>{t("code.details")}</strong>
                </div>
                <dl className="codeproj-page__facts">
                  <div>
                    <dt>{t("code.folderLabel")}</dt>
                    <dd title={display}>{path ? display : t("code.noProject")}</dd>
                  </div>
                  <div>
                    <dt>{t("code.sessions")}</dt>
                    <dd>{sessions.length}</dd>
                  </div>
                  {last && (
                    <div>
                      <dt>{t("code.lastActive")}</dt>
                      <dd>{new Date(last).toLocaleString()}</dd>
                    </div>
                  )}
                </dl>
              </div>
            </aside>
          </div>
          {path && <GitGraph path={path} onCommits={onCommits} linked={{ links, title, filter, onClearFilter: () => setFilter(null) }} />}
        </div>
      </div>
      <Dialog
        open={!!confirm}
        title={t("row.delete")}
        onClose={() => setConfirm(null)}
        footer={
          <>
            <Button onClick={() => setConfirm(null)}>{t("common.cancel")}</Button>
            <Button variant="danger" onClick={() => (setConfirm(null), void app.deleteSession(confirm!).catch(fail))}>
              {t("common.delete")}
            </Button>
          </>
        }
      >
        {t("row.deleteConfirm", { title: app.sessions.get(confirm ?? "")?.title || t("sidebar.untitledSession") })}
      </Dialog>
    </div>
  );
}
