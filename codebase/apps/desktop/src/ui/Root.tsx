import { useEffect, useMemo, useState, type ReactNode } from "react";
import { layoutFor, ShellContext, useWindowWidth, type ShellState } from "./shell";
import { applyAppearance, Banner, Button, Card, Dialog, Icon, Input } from "@genie/ui";
import { GenieMark } from "./GenieMark";
import { t } from "../i18n";
import { preloadOnIdle } from "../markdown/markdown";
import { useApp } from "./context";
import { Sidebar } from "./Sidebar";
import { ChatView } from "./chat/ChatView";
import { PANEL_ICON, RightPanel, effectiveTab } from "./panel/RightPanel";
import { SettingsPage } from "./settings/SettingsPage";
import { FirstRun } from "./FirstRun";
import { ProjectPage, ProjectsPage } from "./ProjectPage";
import { CodeProjectPage } from "./code/CodeProjectPage";
import { HandoffView, useHandoffImport } from "./Handoff";

export function Root() {
  const app = useApp();
  const { appearance } = app.ui;

  useEffect(() => applyAppearance(appearance), [appearance]);
  // The native title bar follows the OS theme unless told otherwise; match the app's choice
  // ("system" hands it back to the OS, which is also what the page follows then).
  useEffect(() => {
    void app.platform.setWindowTheme(appearance.theme === "system" ? null : appearance.theme).catch(() => {});
  }, [app.platform, appearance.theme]);
  // macOS draws its window buttons over our title-bar strip (Overlay title bar), so CSS
  // leaves room for them there only.
  useEffect(() => {
    if (app.info?.os) document.documentElement.dataset.os = app.info.os;
  }, [app.info?.os]);
  useEffect(() => preloadOnIdle(document.documentElement.dataset.theme === "dark"), []);
  // The page frame must never scroll: WebKit scrolls overflow:hidden ancestors to reveal a
  // focused or growing element, which slid the whole UI under the title bar while streaming.
  useEffect(() => {
    const frame = (el: EventTarget | null) => el === document || (el instanceof HTMLElement && el.matches("html, body, #root, .shell, .main, .sidebar, .chat, .panel"));
    const onScroll = (e: Event) => {
      if (!frame(e.target)) return;
      const el = e.target === document ? document.scrollingElement : (e.target as HTMLElement);
      if (el && (el.scrollTop || el.scrollLeft)) el.scrollTop = el.scrollLeft = 0;
    };
    document.addEventListener("scroll", onScroll, true);
    return () => document.removeEventListener("scroll", onScroll, true);
  }, []);

  // Links in rendered Markdown open in the Browser tab when "Open links in built-in browser"
  // is on and a session is open (t47), else in the system browser (always with Cmd/Ctrl);
  // code blocks copy (05 §2.2).
  useEffect(() => {
    const onClick = (e: MouseEvent) => {
      const el = e.target as HTMLElement;
      const preview = el.closest<HTMLElement>("[data-preview]");
      if (preview && app.ui.activeSession) {
        e.preventDefault();
        app.openFile(app.ui.activeSession, preview.dataset.preview!);
        return;
      }
      const link = el.closest<HTMLElement>("[data-href]");
      if (link) {
        e.preventDefault();
        const href = link.dataset.href!;
        const external = () => void app.platform.openExternal(href).catch(() => {});
        const sid = app.ui.activeSession;
        if (href.startsWith("genie://")) app.openDeepLink(href);
        else if (sid && !app.ui.browserLinkAsked && /^https?:\/\//i.test(href) && !(e.metaKey || e.ctrlKey)) {
          // The first outside link asks where links should open (t47).
          app.askLinks(sid, href);
        } else if (sid && app.ui.browserOpenLinks && /^https?:\/\//i.test(href) && !(e.metaKey || e.ctrlKey)) {
          app.setUi({ panelOpen: true, panelTab: "browser" });
          // No browser installed, or the daemon refused: fall back to the system browser.
          void app.client.browser.tab.new({ session_id: sid, url: href }).catch(external);
        } else external();
        return;
      }
      const copy = el.closest<HTMLElement>(".code-copy");
      if (copy) {
        void navigator.clipboard?.writeText(decodeURIComponent(copy.dataset.copy ?? "")).then(() => {
          copy.textContent = t("chat.copied");
          setTimeout(() => (copy.textContent = t("chat.copy")), 1200);
        });
      }
    };
    document.addEventListener("click", onClick);
    return () => document.removeEventListener("click", onClick);
  }, [app]);

  if (!app.stateLoaded) return <Splash text={t("conn.connecting")} />;
  const st = app.status;
  if (st.state === "failed")
    return (
      <Center>
        <Card>
          <h2 style={{ marginTop: 0 }}>{t("conn.failed")}</h2>
          <p className="muted">{st.detail}</p>
          <div className="row">
            <Button variant="primary" icon="refresh" onClick={() => void app.platform.retry()}>
              {t("conn.retry")}
            </Button>
            <Button onClick={() => app.openDeepLink("genie://settings/developer")}>{t("conn.openLogs")}</Button>
          </div>
        </Card>
      </Center>
    );
  if (st.state === "busy_updating") return <Splash text={t("conn.busy", { n: st.running ?? 0 })} />;
  if (st.locked) return <Unlock />;
  if (st.storage && !app.storageDismissed) return <StorageProblemScreen />;
  const ready = st.state === "ready" || st.state === "degraded";
  if (!ready && !app.models.length && !app.sessions.size) return <Splash text={st.state === "starting" ? t("conn.starting") : st.state === "restarting" ? t("conn.restarting") : t("conn.connecting")} />;
  if (!app.ui.firstRunDone) return <FirstRun />;
  return <Shell />;
}

function Shell() {
  const app = useApp();
  const { ui, status } = app;
  const width = useWindowWidth();
  const handoffImport = useHandoffImport(app);
  useEffect(() => {
    app.pickHandoffFile = handoffImport.open;
    return () => {
      app.pickHandoffFile = null;
    };
  }, [app, handoffImport.open]);
  const panelShown = ui.view !== "project" && ui.view !== "handoff" && ui.panelOpen && !ui.panelMinimized;
  const panelPill = ui.view !== "project" && ui.view !== "handoff" && ui.panelOpen && ui.panelMinimized;
  const { narrow, overlay, panelWidth } = layoutFor(width, { sidebarWidth: ui.sidebarWidth, panelOpen: panelShown, panelWidth: ui.panelWidth });
  const [drawerOpen, setDrawer] = useState(false);
  // The drawer is transient: it closes when the layout widens or the user goes somewhere.
  useEffect(() => {
    if (!overlay) setDrawer(false);
  }, [overlay]);
  useEffect(() => setDrawer(false), [ui.activeSession, ui.activeProject, ui.view, ui.mode]);
  useEffect(() => {
    if (!drawerOpen) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setDrawer(false);
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [drawerOpen]);
  // Cmd/Ctrl+Shift+B shows or hides the Browser tab (as in Claude Code Desktop, t47).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Cmd/Ctrl+Shift+S: Element select in the Browser tab (ticket 47).
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && !e.altKey && e.key.toLowerCase() === "s" && app.ui.activeSession) {
        e.preventDefault();
        app.setSelectingElement(!app.selectingElement);
        return;
      }
      if (!(e.metaKey || e.ctrlKey) || !e.shiftKey || e.altKey || e.key.toLowerCase() !== "b") return;
      e.preventDefault();
      const shown = app.ui.panelOpen && app.ui.panelTab === "browser";
      app.setUi(shown ? { panelOpen: false } : { panelOpen: true, panelTab: "browser" });
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [app]);
  const sidebarHidden = overlay ? !drawerOpen : ui.sidebarCollapsed;
  const shell = useMemo<ShellState>(() => ({ width, overlay, narrow, drawerOpen, setDrawer, sidebarHidden }), [width, overlay, narrow, drawerOpen, sidebarHidden]);
  const cls = ["shell", overlay ? "shell--overlay" : ui.sidebarCollapsed ? "shell--collapsed" : "", overlay && drawerOpen ? "shell--drawer" : "", narrow ? "shell--narrow" : ""].filter(Boolean).join(" ");
  return (
    <ShellContext.Provider value={shell}>
    <div className={cls} style={{ ["--sidebar-w" as string]: `${ui.sidebarWidth}px`, ["--panel-w" as string]: `${panelWidth}px` }}>
      <div className="sidebar-slot">
        <Sidebar hidden={sidebarHidden} />
      </div>
      {overlay && <div className="drawer-scrim" aria-hidden onClick={() => setDrawer(false)} />}
      <main className="main">
        {status.state === "reconnecting" && (
          <div className="top-banner">
            <Banner tone="warning" icon="refresh">
              {t("conn.reconnecting")}
            </Banner>
          </div>
        )}
        {status.storage && (
          <div className="top-banner">
            <Banner
              tone="danger"
              icon="warning"
              actions={
                <Button size="sm" onClick={() => app.setStorageDismissed(false)}>
                  {t("storage.fix")}
                </Button>
              }
            >
              {t("storage.banner")}
            </Banner>
          </div>
        )}
        {status.state === "degraded" && (
          <div className="top-banner">
            <Banner tone="neutral">{t("conn.degraded")}</Banner>
          </div>
        )}
        {ui.view === "handoff" ? (
          <HandoffView />
        ) : ui.view === "project" ? (
          ui.activeProject ? <ProjectPage id={ui.activeProject} /> : <ProjectsPage />
        ) : ui.view === "folder" && ui.mode === "code" ? (
          <CodeProjectPage key={ui.codeWorkspace} path={ui.codeWorkspace} />
        ) : (
          <ChatView key={ui.activeSession ?? "new"} id={ui.activeSession} />
        )}
      </main>
      {panelShown && <RightPanel />}
      {panelPill && (
        <button className="panel-pill" onClick={() => app.setUi({ panelMinimized: false })} title={t("panel.restore")}>
          <Icon name={PANEL_ICON[effectiveTab(ui.mode, ui.panelTab)]} size={14} />
          <span>{t(`panel.${effectiveTab(ui.mode, ui.panelTab)}` as Parameters<typeof t>[0])}</span>
          <Icon name="chevronLeft" size={12} />
        </button>
      )}
      <Dialog
        open={!!app.linkAsk}
        title={t("browser.linkAskTitle")}
        onClose={() => app.answerLinks(null)}
        footer={
          <>
            <Button onClick={() => app.answerLinks(false)}>{t("browser.linkAskSystem")}</Button>
            <Button variant="primary" icon="globe" onClick={() => app.answerLinks(true)}>
              {t("browser.linkAskPane")}
            </Button>
          </>
        }
      >
        <p>{t("browser.linkAskBody")}</p>
        {app.linkAsk && <p className="muted link-ask__url">{app.linkAsk.href}</p>}
      </Dialog>
      {ui.view === "settings" && <SettingsPage />}
      <Toasts />
      {handoffImport.input}
    </div>
    </ShellContext.Provider>
  );
}

function Toasts() {
  const app = useApp();
  if (!app.toasts.length) return null;
  return (
    <div className="toasts" role="status" aria-live="polite">
      {app.toasts.map((toast) => (
        <div key={toast.id} className={`toast toast--${toast.tone}`}>
          <div className="toast__body">
            <strong>{toast.title}</strong>
            {toast.body && <div className="muted">{toast.body}</div>}
          </div>
          {toast.link && (
            <Button size="sm" onClick={() => (app.openDeepLink(toast.link!), app.dismissToast(toast.id))}>
              {t("common.open")}
            </Button>
          )}
          <Button size="sm" variant="ghost" onClick={() => app.dismissToast(toast.id)} aria-label={t("common.close")}>
            ×
          </Button>
        </div>
      ))}
    </div>
  );
}

function Unlock() {
  const app = useApp();
  const [pass, setPass] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submit = async () => {
    setBusy(true);
    try {
      const r = await app.client.secrets.unlock({ passphrase: pass });
      if (!r.ok) setErr(t("unlock.wrong"));
      else {
        app.markUnlocked();
      }
    } catch (e) {
      setErr(app.describeError(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Center>
      <Card>
        <h2 style={{ marginTop: 0 }}>{t("unlock.title")}</h2>
        <p className="muted">{t("unlock.body")}</p>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
          className="stack"
        >
          <Input type="password" aria-label={t("unlock.passphrase")} value={pass} onChange={(e) => setPass(e.target.value)} autoFocus />
          {err && <div className="error-text">{err}</div>}
          <Button type="submit" variant="primary" disabled={!pass || busy}>
            {t("unlock.submit")}
          </Button>
        </form>
      </Card>
    </Center>
  );
}

/** The engine runs without its database (04 §2): say so before anything else, and offer
 *  the recovery key, a fresh start (old files set aside, never deleted) or going on
 *  unsaved. Both fixes restart the engine; the relay reconnects by itself. */
function StorageProblemScreen() {
  const app = useApp();
  const problem = app.status.storage!;
  const keyMissing = problem.reason === "key_missing";
  const [key, setKey] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirmFresh, setConfirmFresh] = useState(false);
  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setErr(null);
    try {
      await f();
    } catch (e) {
      setErr(app.describeError(e));
      setBusy(false);
    }
  };
  if (busy && !err) return <Splash text={t("storage.restarting")} />;
  return (
    <Center>
      <Card>
        <h2 style={{ marginTop: 0 }}>{t("storage.title")}</h2>
        <p className="muted">{keyMissing ? t("storage.keyMissing") : t("storage.openFailed", { detail: problem.detail })}</p>
        {keyMissing && problem.recovery_available && (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              void run(() => app.client.storage.recover({ recovery_key: key }));
            }}
            className="stack"
          >
            <Input aria-label={t("storage.recoveryLabel")} placeholder="XXXX-XXXX-…" value={key} onChange={(e) => setKey(e.target.value)} autoFocus />
            <Button type="submit" variant="primary" disabled={!key.trim() || busy}>
              {t("storage.recover")}
            </Button>
          </form>
        )}
        {err && <div className="error-text">{err}</div>}
        {confirmFresh ? (
          <div className="stack">
            <p>{t("storage.startFreshConfirm")}</p>
            <div className="row">
              <Button variant="danger" disabled={busy} onClick={() => void run(() => app.client.storage.start_fresh())}>
                {t("storage.startFreshYes")}
              </Button>
              <Button variant="ghost" onClick={() => setConfirmFresh(false)}>
                {t("common.cancel")}
              </Button>
            </div>
          </div>
        ) : (
          <div className="row">
            {keyMissing && <Button onClick={() => setConfirmFresh(true)}>{t("storage.startFresh")}</Button>}
            <Button variant="ghost" onClick={() => app.setStorageDismissed(true)}>
              {t("storage.continue")}
            </Button>
            <Button variant="ghost" onClick={() => (app.setStorageDismissed(true), app.openDeepLink("genie://settings/developer"))}>
              {t("conn.openLogs")}
            </Button>
          </div>
        )}
      </Card>
    </Center>
  );
}

export function Splash({ text }: { text: string }) {
  return (
    <Center>
      <div className="row" role="status">
        <GenieMark size={28} state="thinking" />
        <span>{text}</span>
      </div>
    </Center>
  );
}

function Center({ children }: { children: ReactNode }) {
  return <div className="center-screen">{children}</div>;
}
