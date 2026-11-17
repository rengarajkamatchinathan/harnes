// Main column (05 §2.2, §3.2): header, banners, transcript, ask cards, composer.
import { useEffect, useState } from "react";
import type { Mode } from "@genie/protocol";
import { Banner, Button, Icon, IconButton, Select } from "@genie/ui";
import { t } from "../../i18n";
import { drivenElsewhere } from "../../state/sessionModel";
import type { MessageView } from "../../state/sessionModel";
import { useApp, useSession } from "../context";
import { AskCard } from "./AskCard";
import { Composer } from "./Composer";
import { Transcript } from "./Transcript";
import { RewindDialog, WorktreeFooter } from "../code/CodeExtras";
import { NewCodeSession } from "../code/NewCodeSession";

const MODES: Mode[] = ["default", "accept_edits", "plan", "bypass"];

export function ChatView({ id }: { id: string | null }) {
  if (!id) return <EmptyChat />;
  return <SessionView id={id} />;
}

function EmptyChat() {
  const app = useApp();
  const noProvider = app.providers.length > 0 && !app.providers.some((p) => p.status === "ready");
  const kind = app.ui.mode;
  return (
    <div className="chat">
      <div className="chat__header">
        <span className="grow" />
        <PanelToggle />
      </div>
      {app.ui.incognito && kind === "chat" && (
        <div className="chat__banners">
          <Banner tone="neutral" icon="incognito">
            {t("chat.incognitoBanner")}
          </Banner>
        </div>
      )}
      <div className="chat__empty">
        <h1>{kind === "code" ? t("code.newSession") : t("chat.emptyTitle")}</h1>
        {kind === "code" && <NewCodeButton />}
        {noProvider && (
          <Button variant="primary" onClick={() => app.openDeepLink("genie://settings/providers")}>
            {t("chat.addProvider")}
          </Button>
        )}
      </div>
      {kind === "chat" && (
        <div className="chat__bottom">
          <Composer
            store={null}
            onCreate={async () => {
              const id = await app.createSession({ kind: "chat", projectId: app.ui.activeProject });
              const s = app.session(id);
              await waitLoaded(s);
              return s;
            }}
          />
        </div>
      )}
    </div>
  );
}

function NewCodeButton() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <Button variant="primary" icon="folder" onClick={() => setOpen(true)}>
        {t("code.newSession")}
      </Button>
      {open && <NewCodeSession onClose={() => setOpen(false)} />}
    </>
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

function PanelToggle() {
  const app = useApp();
  return <IconButton icon="panel" label={app.ui.panelOpen ? t("panel.hide") : t("panel.show")} aria-pressed={app.ui.panelOpen} onClick={() => app.setUi({ panelOpen: !app.ui.panelOpen })} />;
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

  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });
  const asks = [...m.asks.values()];
  const statusText =
    m.status === "awaiting_ask" ? t("chat.statusAsk") : m.status === "paused" ? t("chat.statusPaused") : m.status === "compacting" ? t("chat.statusCompacting") : m.status === "running" ? t("chat.statusRunning") : null;

  return (
    <div className="chat">
      <header className="chat__header">
        {code ? (
          <>
            <Icon name="folder" />
            <span className="chat__title" title={info?.workspace_id ?? ""}>
              {(info?.workspace_id ?? "").split(/[\\/]/).filter(Boolean).pop() ?? t("code.repo")}
            </span>
            <Select
              aria-label={t("code.mode")}
              style={{ width: "auto" }}
              value={info?.mode ?? "default"}
              onChange={(v) => void app.client.session.update({ session_id: id, mode: v as Mode }).catch(fail)}
              options={MODES.map((md) => ({ value: md, label: t(`code.mode.${md}` as Parameters<typeof t>[0]) }))}
            />
          </>
        ) : (
          <h1 className="chat__title">{info?.title || t("sidebar.untitled")}</h1>
        )}
        {info && <span className="muted">{info.model}{info.effort ? ` · ${info.effort}` : ""}</span>}
        <span className="grow" />
        {statusText && (
          <span className="row muted" role="status">
            <span className="g-spinner" /> {statusText}
          </span>
        )}
        <PanelToggle />
      </header>

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

      {store.loading && !m.messages.length ? (
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
        {asks.map((a) => (
          <AskCard key={a.id} ask={a} onAnswer={(ans) => store.answer(a, ans)} onBrowserTakeOver={() => (app.setUi({ panelOpen: true, panelTab: "browser" }), void app.client.browser.takeover({ session_id: id }).catch(fail))} />
        ))}
        <Composer store={store} readOnly={!!elsewhere || m.leaseLost} />
        {code && info?.workspace_id && <WorktreeFooter sessionId={id} />}
      </div>
      {rewind && <RewindDialog sessionId={id} message={rewind} onClose={() => setRewind(null)} onDone={() => void store.reload()} />}
    </div>
  );
}
