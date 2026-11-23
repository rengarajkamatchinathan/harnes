// Main column (05 §2.2, §3.2, D54): header (title menu, right-panel buttons), greeting hero or
// Code folder card when empty, banners, transcript, ask cards, composer with model footer.
import { useEffect, useState, type ReactNode } from "react";
import type { Mode } from "@genie/protocol";
import { Banner, Button, Icon, IconButton, Menu, type IconName } from "@genie/ui";
import { t } from "../../i18n";
import { drivenElsewhere } from "../../state/sessionModel";
import type { MessageView } from "../../state/sessionModel";
import { useApp, useSession } from "../context";
import { AskCard } from "./AskCard";
import { Composer, ModelPicker } from "./Composer";
import { SessionUsage } from "./Usage";
import { ContextBanner } from "./ContextMeter";
import { CompareToggle, CompareView } from "./Compare";
import { raceBook } from "../../state/race";
import { useEmitter } from "../../state/emitter";
import { CodeProjectPicker, folderName, NewProject, TitlebarControls } from "../Sidebar";
import { GenieMark } from "../GenieMark";
import { useShell } from "../shell";
import { effectiveTab, PANEL_ICON, panelTabs } from "../panel/RightPanel";
import { sameWorkspace, type PanelTab } from "../../state/app";
import { Transcript } from "./Transcript";
import { RewindDialog, WorktreeFooter } from "../code/CodeExtras";
import { NewCodeSession } from "../code/NewCodeSession";

const MODES: Mode[] = ["default", "accept_edits", "plan", "bypass"];

export function ChatView({ id }: { id: string | null }) {
  if (!id) return <EmptyChat />;
  return <SessionView id={id} />;
}

/** Time-of-day greeting for the empty chat (pure, for tests). */
export function greeting(name: string, now: Date = new Date()): string {
  const h = now.getHours();
  const key = h >= 5 && h < 12 ? "greeting.morning" : h >= 12 && h < 17 ? "greeting.afternoon" : h >= 17 && h < 23 ? "greeting.evening" : "greeting.late";
  const n = name.trim();
  const base = t(key);
  if (!n) return key === "greeting.late" ? `${base}?` : base;
  return key === "greeting.late" ? `${base}, ${n}?` : `${base}, ${n}`;
}

/** Top strip of the main column: drag region, title, right-side panel buttons. */
function Header({ children, right, panels = true, hasArtifacts = false }: { children?: ReactNode; right?: ReactNode; panels?: boolean; hasArtifacts?: boolean }) {
  const shell = useShell();
  return (
    <header className={`chat__header ${shell.sidebarHidden ? "chat__header--collapsed" : ""}`} data-tauri-drag-region>
      {shell.sidebarHidden && <TitlebarControls />}
      {children}
      <span className="grow" data-tauri-drag-region />
      {right}
      {panels && <PanelButtons hasArtifacts={hasArtifacts} />}
    </header>
  );
}


/** One icon per right-panel tab; the active one is pressed (D54). */
function PanelButtons({ hasArtifacts }: { hasArtifacts: boolean }) {
  const app = useApp();
  const { ui } = app;
  // The artifact button only appears once the chat has made one; otherwise it opens an empty panel.
  const order: PanelTab[] = ui.mode === "code" ? ["terminal", "browser", "files", "changes"] : panelTabs(ui.mode).filter((tab) => tab !== "artifact" || hasArtifacts);
  const current = effectiveTab(ui.mode, ui.panelTab);
  return (
    <div className="header-icons">
      {order.map((tab) => {
        const on = ui.panelOpen && current === tab;
        return (
          <IconButton
            key={tab}
            icon={PANEL_ICON[tab]}
            size="sm"
            className={on ? "is-on" : ""}
            aria-pressed={on}
            label={t(`panel.${tab}` as Parameters<typeof t>[0])}
            shortcut={tab === "browser" ? (/Mac/i.test(navigator.platform) ? "⌘⇧B" : "Ctrl+Shift+B") : undefined}
            onClick={() => app.setUi(on ? { panelOpen: false } : { panelOpen: true, panelTab: tab })}
          />
        );
      })}
    </div>
  );
}

function EmptyChat() {
  const app = useApp();
  // Nothing to talk to until some connected provider offers a model (Ollama is "ready"
  // with no key, so provider status alone isn't enough).
  const noProvider = app.providers.length > 0 && !app.usableModels.length;
  return app.ui.mode === "code" ? <EmptyCode noProvider={noProvider} /> : <EmptyHero noProvider={noProvider} />;
}

function EmptyHero({ noProvider }: { noProvider: boolean }) {
  const app = useApp();
  const [seed, setSeed] = useState<{ text: string; n: number } | undefined>();
  const project = app.projects.find((p) => p.project_id === app.ui.activeProject);
  const [creating, setCreating] = useState(false);
  const ideas = ["chat.idea.brief", "chat.idea.plan", "chat.idea.code"] as const;
  const ideaIcon: IconName[] = ["file", "clock", "code"];
  return (
    <div className="chat">
      <Header
        panels={false}
        right={
          <IconButton
            icon="ghost"
            size="sm"
            className={app.ui.incognito ? "is-on" : ""}
            aria-pressed={app.ui.incognito}
            label={t("sidebar.incognito")}
            onClick={() => app.setUi({ incognito: !app.ui.incognito })}
          />
        }
      />
      <div className="hero">
        <div className="hero__inner">
          <h1 className="hero__title">
            <span className="hero__mark" aria-hidden>
              {app.ui.incognito ? <Icon name="ghost" size={30} /> : <GenieMark size={36} />}
            </span>
            {app.ui.incognito ? t("sidebar.incognito") : greeting(app.userName)}
          </h1>
          {app.ui.incognito && <p className="muted hero__sub">{t("chat.incognitoBanner")}</p>}
          <Composer
            layout="hero"
            store={null}
            seed={seed}
            placeholder={project ? t("composer.placeholderProject", { name: project.name }) : t("composer.placeholderEmpty")}
            leading={
              <Menu
                label={t("chat.projectOrFolder")}
                placement="up"
                className="projpill__list"
                items={[
                  { id: "", label: t("row.noProject"), icon: "chat", checked: !project, onSelect: () => app.setUi({ activeProject: null }) },
                  ...app.projects.map((p, i) => ({ id: p.project_id, label: p.name, icon: "folder" as const, separatorBefore: i === 0, checked: p.project_id === project?.project_id, onSelect: () => app.setUi({ activeProject: p.project_id }) })),
                  { id: "new", label: t("sidebar.newProject"), icon: "plus", separatorBefore: true, keepFocus: true, onSelect: () => setCreating(true) },
                ]}
                trigger={(p) => (
                  <button className={`projpill ${project ? "is-set" : ""}`} title={project ? t("chat.inProject", { name: project.name }) : t("chat.pickProject")} {...p}>
                    <Icon name="folder" size={14} />
                    <span className="projpill__name">{project?.name ?? t("chat.projectOrFolder")}</span>
                    <Icon name="chevronDown" size={12} />
                  </button>
                )}
              />
            }
            onCreate={async () => {
              const id = await app.createSession({ kind: "chat", projectId: app.ui.activeProject });
              const s = app.session(id);
              await waitLoaded(s);
              return s;
            }}
          />
          {noProvider ? (
            <div className="hero__ideas">
              <Button variant="primary" onClick={() => app.openDeepLink("genie://settings/providers")}>
                {t("chat.addProvider")}
              </Button>
            </div>
          ) : (
            <div className="hero__ideas">
              <div className="hero__ideas-title">{t("chat.ideas")}</div>
              {ideas.map((k, i) => (
                <button key={k} className="idea" onClick={() => setSeed({ text: t(k), n: (seed?.n ?? 0) + 1 })}>
                  <span className="idea__icon">
                    <Icon name={ideaIcon[i]} />
                  </span>
                  {t(k)}
                </button>
              ))}
            </div>
          )}
        </div>
      </div>
      {creating && <NewProject stay onClose={() => setCreating(false)} />}
    </div>
  );
}

/** Code mode, nothing open: pick a project in the centre ("No project" by default), describe the task below (05 §3.1). */
function EmptyCode({ noProvider }: { noProvider: boolean }) {
  const app = useApp();
  // Shared with the sidebar's project switcher.
  const path = app.ui.codeWorkspace;
  const setPath = (w: string) => app.setUi({ codeWorkspace: w });
  const [dialog, setDialog] = useState(false);
  const pick = () =>
    void app.platform.pickFolder(t("code.folder")).then(
      (p) => {
        if (!p) return;
        app.setUi({ codeWorkspace: p, recentWorkspaces: [p, ...app.ui.recentWorkspaces.filter((w) => !sameWorkspace(w, p))].slice(0, 8) });
      },
      () => {},
    );
  const choices = app.codeWorkspaces.slice(0, 5);
  if (path && !choices.some((w) => sameWorkspace(w, path))) choices.unshift(path);
  return (
    <div className="chat">
      <Header panels={false}>
        <span className="chat__title chat__title--muted">{t("sidebar.newSession")}</span>
      </Header>
      <div className="hero hero--code">
        <div className="folder-card">
          <span className="hero__mark folder-card__mark" aria-hidden>
            <GenieMark size={30} />
          </span>
          <div className="folder-card__box">
            <button className="folder-card__row" onClick={() => setPath("")}>
              <Icon name="chat" />
              <span className="sidebar__title">{t("code.noProject")}</span>
              {!path && <Icon name="check" />}
            </button>
            {choices.map((w) => (
              <button key={w} className="folder-card__row" title={w} onClick={() => setPath(w)}>
                <Icon name="repo" />
                <span className="sidebar__title">{folderName(w)}</span>
                {!!path && sameWorkspace(w, path) && <Icon name="check" />}
              </button>
            ))}
            <button className="folder-card__row folder-card__row--muted" onClick={pick}>
              <Icon name="plus" />
              <span className="sidebar__title">{t("code.openFolder")}</span>
            </button>
          </div>
          {noProvider && (
            <Button variant="primary" onClick={() => app.openDeepLink("genie://settings/providers")}>
              {t("chat.addProvider")}
            </Button>
          )}
        </div>
      </div>
      <div className="chat__bottom">
        <Composer
          store={null}
          placeholder={t("code.placeholder")}
          onCreate={async () => {
            if (path) {
              // Untrusted or unknown folders go through the trust dialog first (D43).
              const trust = await app.client.trust.get({ path }).catch(() => ({ state: "trusted" as const }));
              if (trust.state !== "trusted") {
                setDialog(true);
                return null;
              }
            }
            const id = await app.createSession({ kind: "code", workspacePath: path || undefined });
            const s = app.session(id);
            await waitLoaded(s);
            return s;
          }}
        />
        <div className="composer-foot">
          <span className="composer-foot__item">
            <Icon name="computer" size={14} /> {t("code.thisComputer")}
          </span>
          <CodeProjectPicker app={app} variant="inline" />
          <span className="grow" />
          <ModelPicker store={null} />
        </div>
      </div>
      {dialog && <NewCodeSession initialPath={path} onClose={() => setDialog(false)} />}
    </div>
  );
}

function waitLoaded(s: { loading: boolean; subscribe: (f: () => void) => () => void }): Promise<void> {
  if (!s.loading) return Promise.resolve();
  return new Promise((resolve) => {
    const off = s.subscribe(() => {
      if (!s.loading) {
        off();
        resolve();
      }
    });
    setTimeout(resolve, 5000);
  });
}

function SessionView({ id }: { id: string }) {
  const app = useApp();
  const store = useSession(id)!;
  const m = store.model;
  const info = m.info;
  const code = info?.kind === "code";
  const [rewind, setRewind] = useState<MessageView | null>(null);
  const [now, setNow] = useState(Date.now());
  const elsewhere = drivenElsewhere(m, app.selfId);
  useEmitter(raceBook);
  const race = code ? undefined : raceBook.shown(id);

  // Retry countdown.
  useEffect(() => {
    if (!m.retry) return;
    const h = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(h);
  }, [m.retry]);
  // Open the artifact panel as soon as an artifact appears (D28).
  useEffect(() => {
    if (m.artifacts.length && !code) app.setUi({ panelOpen: true, panelTab: "artifact" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [m.artifacts.length]);
  // Show the Browser tab when Genie starts browsing, and whenever it needs the user there
  // (hand-off, approval) (05 §9).
  const browsing = m.browser.actions.length > 0;
  const browserCtl = m.browser.control?.control;
  useEffect(() => {
    if (browsing || browserCtl === "handoff" || browserCtl === "waiting_for_approval") app.setUi({ panelOpen: true, panelTab: "browser" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [browsing, browserCtl === "handoff" || browserCtl === "waiting_for_approval"]);

  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });
  const exportAs = async (format: "markdown" | "json") => {
    try {
      const r = await app.client.session.export({ session_id: id, format });
      const name = `${(info?.title ?? "chat").slice(0, 60)}.${format === "markdown" ? "md" : "json"}`;
      const path = await app.platform.saveFile(name, { text: r.text ?? "" });
      app.toast({ title: t("row.exported", { path }), tone: "success" });
    } catch (e) {
      fail(e);
    }
  };
  const asks = [...m.asks.values()];
  const statusText =
    // Running and compacting show in the transcript's turn indicator; the header only flags waits.
    m.status === "awaiting_ask" ? t("chat.statusAsk") : m.status === "paused" ? t("chat.statusPaused") : null;

  return (
    <div className="chat">
      <Header
        hasArtifacts={m.artifacts.length > 0}
        right={
          statusText && (
            <span className="row muted header-status" role="status">
              <Icon name="hand" size={14} /> {statusText}
            </span>
          )
        }
      >
        {code ? (
          <span className="chat__title" title={info?.workspace_id ?? ""}>
            {info?.title || folderName(info?.workspace_id ?? "") || t("code.repo")}
          </span>
        ) : (
          <Menu
            label={t("chat.options")}
            items={[
              { id: "pin", label: info?.pinned ? t("row.unpin") : t("row.pin"), icon: "pin", onSelect: () => void app.client.session.update({ session_id: id, pinned: !info?.pinned }).catch(fail) },
              { id: "md", label: t("row.exportMd"), icon: "download", onSelect: () => void exportAs("markdown") },
              { id: "json", label: t("row.exportJson"), icon: "download", onSelect: () => void exportAs("json") },
            ]}
            trigger={(p) => (
              <button className="titlebtn" {...p}>
                <h1 className="chat__title">{info?.title || firstPrompt(m.messages) || t("sidebar.untitled")}</h1>
                <Icon name="chevronDown" size={14} />
              </button>
            )}
          />
        )}
      </Header>

      <div className="chat__banners">
        {info?.incognito && (
          <Banner tone="neutral" icon="incognito">
            {t("chat.incognitoBanner")}
          </Banner>
        )}
        {(elsewhere || m.leaseLost) && (
          <Banner
            tone="warning"
            icon="lock"
            actions={
              <Button size="sm" onClick={() => void store.takeOver().catch(fail)}>
                {t("chat.takeOver")}
              </Button>
            }
          >
            {m.leaseLost
              ? t("chat.leaseLost", { client: m.driver?.label ?? m.driver?.client_kind ?? "Another client" })
              : t("chat.drivenElsewhere", { client: elsewhere?.label ?? elsewhere?.client_kind ?? "" })}
          </Banner>
        )}
        {m.interrupted && (
          <Banner
            tone="warning"
            actions={
              <Button size="sm" onClick={() => void store.resume().catch(fail)}>
                {t("chat.resume")}
              </Button>
            }
          >
            {t("chat.interrupted")}
          </Banner>
        )}
        {m.retry && (
          <Banner
            tone="warning"
            icon="refresh"
            actions={
              <>
                <Button size="sm" onClick={() => void store.resume().catch(fail)}>
                  {t("chat.retryNow")}
                </Button>
                <Button size="sm" variant="ghost" onClick={() => void store.interrupt().catch(fail)}>
                  {t("chat.stop")}
                </Button>
              </>
            }
          >
            {t("chat.retrying", { reason: m.retry.error_class === "rate_limit" ? "Rate limited" : m.retry.error_class, s: Math.max(0, Math.ceil((m.retry.at - now) / 1000)) })}
          </Banner>
        )}
        {m.error && (
          <Banner tone="danger" icon="warning">
            {t("chat.error", { message: m.error.message })}
          </Banner>
        )}
        {store.loadError && (
          <Banner tone="danger" actions={<Button size="sm" onClick={() => void store.reload()}>{t("common.retry")}</Button>}>
            {store.loadError}
          </Banner>
        )}
      </div>

      {race ? (
        <CompareView store={store} race={race} />
      ) : store.loading && !m.messages.length ? (
        <div className="chat__empty muted">{t("common.loading")}</div>
      ) : (
        <Transcript store={store} compact={code} onRewind={code ? setRewind : undefined} />
      )}

      <div className="chat__bottom">
        {m.subagents.size > 0 && (
          <div className="subagents">
            {[...m.subagents.values()].map((s) => (
              <div key={s.childSessionId} className="subagent">
                <Icon name="sparkle" /> <strong>{s.agent}</strong> <span className="muted">{s.description}</span>
                <span className="grow" />
                <span className="muted">{s.status === "running" ? <span className="g-spinner" /> : s.status}</span>
              </div>
            ))}
          </div>
        )}
        {asks.map((a, i) => (
          <AskCard
            key={a.id}
            ask={a}
            onAnswer={(ans) => store.answer(a, ans)}
            onBrowserTakeOver={() => (app.setUi({ panelOpen: true, panelTab: "browser" }), void app.client.browser.takeover({ session_id: id }).catch(fail))}
            permission={{
              autoFocus: i === 0,
              project: code ? folderName(info?.workspace_id ?? "") || undefined : undefined,
              mode: code ? (info?.mode ?? "default") : undefined,
              onSetMode: code && !a.subagent ? (md) => app.client.session.update({ session_id: id, mode: md }).then(() => undefined) : undefined,
            }}
          />
        ))}
        <ContextBanner store={store} readOnly={!!elsewhere || m.leaseLost} />
        <Composer store={store} readOnly={!!elsewhere || m.leaseLost} placeholder={code ? t("code.placeholder") : t("composer.placeholderReply")} />
        <div className="composer-foot">
          {code ? (
            <>
              <span className="composer-foot__item">
                <Icon name="computer" size={14} /> {t("code.thisComputer")}
              </span>
              {folderName(info?.workspace_id ?? "") && (
                <span className="composer-foot__item" title={info?.workspace_id ?? ""}>
                  <Icon name="repo" size={14} /> {folderName(info?.workspace_id ?? "")}
                </span>
              )}
            </>
          ) : (
            <span className="composer-foot__item composer-foot__note">{t("chat.disclaimer")}</span>
          )}
          <span className="grow" />
          {!code && !elsewhere && !m.leaseLost && <CompareToggle store={store} />}
          <ModelPicker store={store} />
          {code && (
            <Menu
              label={t("code.mode")}
              placement="up"
              align="right"
              items={MODES.map((md) => ({ id: md, label: t(`code.mode.${md}` as Parameters<typeof t>[0]), checked: (info?.mode ?? "default") === md, onSelect: () => void app.client.session.update({ session_id: id, mode: md }).catch(fail) }))}
              trigger={(p) => (
                <button className="textbtn" aria-label={t("code.mode")} {...p}>
                  {t(`code.mode.${info?.mode ?? "default"}` as Parameters<typeof t>[0])}
                </button>
              )}
            />
          )}
          <SessionUsage store={store} code={code} />
        </div>
        {code && info?.workspace_id && <WorktreeFooter sessionId={id} />}
      </div>
      {rewind && <RewindDialog sessionId={id} message={rewind} onClose={() => setRewind(null)} onDone={() => void store.reload()} />}
    </div>
  );
}

/** Until the engine names the chat, the header shows what you asked (first line, trimmed). */
function firstPrompt(messages: MessageView[]): string {
  const first = messages.find((x) => x.role === "user");
  const text = first?.parts.find((p) => p.kind === "text")?.text ?? "";
  return text.split("\n")[0].trim().slice(0, 80);
}
