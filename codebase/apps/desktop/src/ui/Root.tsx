import { useEffect, useState } from "react";
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
  useEffect(() => preloadOnIdle(document.documentElement.dataset.theme === "dark"), []);

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
    <div className="shell" style={{ ["--sidebar-w" as string]: `${ui.sidebarWidth}px`, ["--panel-w" as string]: `${ui.panelWidth}px` }}>
      <Sidebar />
      <main className="main" aria-label={ui.view === "settings" ? t("settings.title") : undefined}>
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
        {ui.view === "settings" ? <SettingsPage /> : ui.view === "project" && ui.activeProject ? <ProjectPage id={ui.activeProject} /> : <ChatView key={ui.activeSession ?? "new"} id={ui.activeSession} />}
      </main>
      {ui.view === "main" && ui.panelOpen && <RightPanel />}
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
        app.status = { ...app.status, locked: undefined };
        await app.platform.retry();
        app.changedNow();
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

function Center({ children }: { children: React.ReactNode }) {
  return <div className="center-screen">{children}</div>;
}
