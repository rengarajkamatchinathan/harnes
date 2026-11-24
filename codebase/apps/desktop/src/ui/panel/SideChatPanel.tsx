// Side chat (panel tab, ⌘;): a separate chat beside a Code session, for questions that should
// not interrupt or clutter the main session. One per Code session, created on the first send
// and kept in `UiState.sideChats`; it stays out of the sidebar's chat list.
import { EmptyState } from "@genie/ui";
import { t } from "../../i18n";
import { useApp, useSession } from "../context";
import { Composer } from "../chat/Composer";
import { Transcript } from "../chat/Transcript";
import type { SessionStore } from "../../state/session";

export function SideChatPanel({ sessionId }: { sessionId: string }) {
  const app = useApp();
  const sideId = app.ui.sideChats[sessionId] ?? null;
  // A side chat deleted elsewhere falls back to a fresh one.
  const known = sideId ? app.sessions.has(sideId) : false;
  const store = useSession(known ? sideId : null);

  const create = async (): Promise<SessionStore> => {
    const id = await app.createSession({ kind: "chat", select: false });
    app.setUi({ sideChats: { ...app.ui.sideChats, [sessionId]: id } });
    const s = app.session(id);
    await waitLoaded(s);
    return s;
  };

  return (
    <div className="sidechat">
      <div className="sidechat__body">
        {store && store.model.messages.length > 0 ? (
          <Transcript store={store} compact />
        ) : (
          <EmptyState icon="chat" title={t("sidechat.title")} hint={t("sidechat.hint")} />
        )}
      </div>
      <div className="sidechat__composer">
        <Composer store={store} onCreate={store ? undefined : create} placeholder={t("sidechat.placeholder")} />
      </div>
    </div>
  );
}

function waitLoaded(s: SessionStore): Promise<void> {
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
