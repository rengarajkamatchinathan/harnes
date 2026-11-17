import { useEffect, useState, type ReactNode } from "react";
import { applyAppearance, Banner, Button, Card, Input, Spinner } from "@genie/ui";
import { t } from "../i18n";
import { preloadOnIdle } from "../markdown/markdown";
import { useApp } from "./context";
import { Sidebar } from "./Sidebar";
import { ChatView } from "./chat/ChatView";
import { RightPanel } from "./panel/RightPanel";
import { SettingsPage } from "./settings/SettingsPage";
import { FirstRun } from "./FirstRun";
import { ProjectPage } from "./ProjectPage";

export function Root() {
  const app = useApp();
  const { appearance } = app.ui;

  useEffect(() => applyAppearance(appearance), [appearance]);
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

  // Links in rendered Markdown open in the system browser; code blocks copy (05 §2.2).
  useEffect(() => {
    const onClick = (e: MouseEvent) => {
      const el = e.target as HTMLElement;
      const link = el.closest<HTMLElement>("[data-href]");
      if (link) {
        e.preventDefault();
        const href = link.dataset.href!;
        if (href.startsWith("genie://")) app.openDeepLink(href);
        else void app.platform.openExternal(href).catch(() => {});
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
  const ready = st.state === "ready" || st.state === "degraded";
  if (!ready && !app.models.length && !app.sessions.size) return <Splash text={st.state === "starting" ? t("conn.starting") : st.state === "restarting" ? t("conn.restarting") : t("conn.connecting")} />;
  if (!app.ui.firstRunDone) return <FirstRun />;
  return <Shell />;
}

function Shell() {
  const app = useApp();
  const { ui, status } = app;
  return (
    <div className={`shell ${ui.sidebarCollapsed ? "shell--collapsed" : ""}`} style={{ ["--sidebar-w" as string]: `${ui.sidebarWidth}px`, ["--panel-w" as string]: `${ui.panelWidth}px` }}>
      {!ui.sidebarCollapsed && <Sidebar />}
      <main className="main">
        {status.state === "reconnecting" && (
          <div className="top-banner">
            <Banner tone="warning" icon="refresh">
              {t("conn.reconnecting")}
            </Banner>
          </div>
        )}
        {status.state === "degraded" && (
          <div className="top-banner">
            <Banner tone="neutral">{t("conn.degraded")}</Banner>
          </div>
        )}
        {ui.view === "project" && ui.activeProject ? <ProjectPage id={ui.activeProject} /> : <ChatView key={ui.activeSession ?? "new"} id={ui.activeSession} />}
      </main>
      {ui.view !== "project" && ui.panelOpen && <RightPanel />}
      {ui.view === "settings" && <SettingsPage />}
      <Toasts />
    </div>
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

export function Splash({ text }: { text: string }) {
  return (
    <Center>
      <div className="row" role="status">
        <Spinner />
        <span>{text}</span>
      </div>
    </Center>
  );
}

function Center({ children }: { children: ReactNode }) {
  return <div className="center-screen">{children}</div>;
}
