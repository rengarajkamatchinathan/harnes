// One open session: `session.get` snapshot + `session.subscribe` from its `head_seq`,
// events applied to the view model, user actions sent as protocol calls (02 §3, §6.2).
import type { Ask, AskAnswer, Delivery, InputPart, SessionEventEnvelope } from "@genie/protocol";
import { Emitter } from "./emitter";
import { applyEvent, emptyModel, isBusy, loadSnapshot, type MessageView, type SessionModel } from "./sessionModel";
import type { App } from "./app";

let msgCounter = 0;
export const newClientMsgId = () => `cmsg_${Date.now().toString(36)}_${(++msgCounter).toString(36)}_${Math.random().toString(36).slice(2, 8)}`;

export class SessionStore extends Emitter {
  model: SessionModel;
  loading = true;
  loadError: string | null = null;
  private closed = false;

  constructor(
    private app: App,
    readonly id: string,
  ) {
    super();
    this.model = emptyModel(id);
  }

  /** Snapshot, then subscribe from its head (no gap: the subscribe replays after it). */
  async open(leafId?: string | null) {
    this.loading = true;
    this.changedNow();
    try {
      const snap = await this.app.client.session.get({ session_id: this.id, leaf_id: leafId ?? null });
      if (this.closed) return;
      loadSnapshot(this.model, snap);
      const sub = await this.app.client.session.subscribe({ session_id: this.id, after_seq: snap.session.head_seq });
      this.model.driver = sub.driver ?? null;
      this.model.status = sub.status;
      try {
        const pending = await this.app.client.permission.pending({ session_id: this.id });
        for (const a of pending.asks) this.model.asks.set(a.id, a);
      } catch {
        /* older engines: asks still arrive as events */
      }
      this.loadError = null;
    } catch (e) {
      this.loadError = this.app.describeError(e);
    } finally {
      this.loading = false;
      this.model.rev++;
      this.changedNow();
    }
  }

  /** Below-floor resync or branch switch: reload the snapshot and resubscribe. */
  reload(leafId?: string | null) {
    return this.open(leafId ?? this.model.info?.active_leaf_id ?? null);
  }

  apply(env: SessionEventEnvelope) {
    if (applyEvent(this.model, env)) {
      this.changed();
      if (this.model.needsReload) {
        this.model.needsReload = false;
        void this.reload();
      }
    }
  }

  markSynced() {
    this.model.synced = true;
    this.changed();
  }

  /** The server asked this client directly (we hold the lease): make sure the card shows. */
  addAsk(ask: Ask) {
    if (!this.model.asks.has(ask.id)) {
      this.model.asks = new Map(this.model.asks).set(ask.id, ask);
      this.model.rev++;
      this.changedNow();
    }
  }

  dropAsk(askId: string) {
    if (this.model.asks.has(askId)) {
      const next = new Map(this.model.asks);
      next.delete(askId);
      this.model.asks = next;
      this.model.rev++;
      this.changedNow();
    }
  }

  onLeaseLost(newDriver: SessionModel["driver"]) {
    this.model.leaseLost = true;
    this.model.driver = newDriver;
    this.model.rev++;
    this.changedNow();
  }

  /** Enter: send, or steer while a turn runs; Alt+Enter: queue (SA-06). */
  async send(parts: InputPart[], delivery?: Delivery, parentId?: string | null) {
    const d: Delivery = delivery ?? "steer";
    const res = await this.app.client.session.prompt({ session_id: this.id, client_msg_id: newClientMsgId(), delivery: d, parts, parent_id: parentId ?? null });
    this.model.leaseLost = false;
    return res;
  }

  /** Edit = prompt with `parent_id` set to the edited message's parent (D24). */
  edit(message: MessageView, text: string) {
    return this.send([{ type: "text", text }], "steer", message.parentId);
  }

  regenerate(messageId: string) {
    return this.app.client.session.regenerate({ session_id: this.id, message_id: messageId });
  }

  interrupt() {
    return this.app.client.session.interrupt({ session_id: this.id });
  }

  resume() {
    this.model.interrupted = null;
    this.changedNow();
    return this.app.client.session.resume({ session_id: this.id });
  }

  async takeOver() {
    await this.app.client.session.take_over({ session_id: this.id });
    this.model.leaseLost = false;
    this.model.driver = { client_instance_id: this.app.info?.client_instance_id ?? "", client_kind: "desktop", label: "Desktop" };
    this.model.rev++;
    this.changedNow();
  }

  async switchBranch(siblingId: string) {
    await this.app.client.session.update({ session_id: this.id, active_leaf_id: siblingId });
    await this.reload(siblingId);
  }

  removeQueued(inboxId: string) {
    return this.app.client.session.inbox.remove({ session_id: this.id, inbox_id: inboxId });
  }

  answer(ask: Ask, answer: AskAnswer) {
    return this.app.answerAsk(ask, answer);
  }

  get busy() {
    return isBusy(this.model);
  }

  async close() {
    this.closed = true;
    try {
      await this.app.client.session.unsubscribe({ session_id: this.id });
    } catch {
      /* connection gone */
    }
  }
}
