// Terminal tabs (05 §10, D55; 02 §6.7): the client side of `terminal.*`.
//
// - `open` starts a shell in the session folder/worktree (`terminal.open`); at most 8 per
//   session (SA-25).
// - A view (an xterm.js instance) attaches with a sink. `terminal.attach {offset}` returns
//   replay bytes from the daemon's ring, or `{truncated, checkpoint_b64}` when the offset fell
//   out of it (the view resets and writes the VT checkpoint). Then ephemeral
//   `terminal.output {start, end, data_b64}` events stream to this connection; anything the
//   replay already covered is skipped, and a gap re-attaches from the last offset.
// - A new view (tab switch, window reload) attaches from offset 0; after a reconnect the live
//   view re-attaches from its offset (no duplicate output).
// - `terminal.exited` (workspace feed) marks the terminal exited; its scrollback stays
//   readable from a local copy because the daemon forgets exited terminals.
// - Terminal ids per session are kept in `state.json` (`UiState.terminals`) so a reload can
//   reattach to shells that are still running.
import type { GenieClient, TerminalExited, TerminalOutput } from "@genie/protocol";
import { errorKind } from "../platform";
import { Emitter } from "./emitter";

/** SA-25. */
export const MAX_TERMINALS = 8;
/** Local scrollback kept per terminal (shown after exit / for exited terminals). */
const LOCAL_CAP = 512 * 1024;

export interface TermInfo {
  id: string;
  sessionId: string;
  title: string;
  exited: boolean;
  code: number | null;
  /** Why the terminal is gone without an exit code (engine restarted, closed elsewhere). */
  lost?: string;
}

/** What a view (xterm.js) implements. */
export interface TermSink {
  /** Clear the screen and state (`ESC c`) before a checkpoint is written. */
  reset(): void;
  write(bytes: Uint8Array): void;
}

interface View {
  sink: TermSink;
  offset: number;
  attaching: boolean;
  pending: TerminalOutput[];
}

interface Host {
  readonly client: GenieClient;
  persistTerminals(map: Record<string, { id: string; title: string }[]>): void;
}

export function b64ToBytes(b64: string): Uint8Array {
  if (!b64) return new Uint8Array(0);
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export function bytesToB64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

const enc = new TextEncoder();

export class Terminals extends Emitter {
  private bySession = new Map<string, TermInfo[]>();
  private views = new Map<string, View>();
  private local = new Map<string, Uint8Array[]>();
  private localSize = new Map<string, number>();
  private titles = 0;

  constructor(private host: Host) {
    super();
  }

  /** Restore ids saved in state.json (reattached lazily when a view mounts). */
  restore(saved: Record<string, { id: string; title: string }[]> | undefined) {
    for (const [sid, list] of Object.entries(saved ?? {})) {
      if (!Array.isArray(list) || !list.length) continue;
      this.bySession.set(
        sid,
        list.map((t) => ({ id: t.id, sessionId: sid, title: t.title, exited: false, code: null })),
      );
    }
  }

  list(sessionId: string): TermInfo[] {
    return this.bySession.get(sessionId) ?? [];
  }

  get(id: string): TermInfo | undefined {
    for (const l of this.bySession.values()) {
      const t = l.find((x) => x.id === id);
      if (t) return t;
    }
    return undefined;
  }

  async open(sessionId: string, cols: number, rows: number, opts: { cwd?: string | null; shell?: string | null } = {}): Promise<TermInfo> {
    const live = this.list(sessionId).filter((t) => !t.exited);
    if (live.length >= MAX_TERMINALS) throw new Error(`At most ${MAX_TERMINALS} terminals per session.`);
    const r = await this.host.client.terminal.open({ session_id: sessionId, cols: Math.max(2, cols), rows: Math.max(1, rows), cwd: opts.cwd ?? null, shell: opts.shell ?? null });
    const info: TermInfo = { id: r.terminal_id, sessionId, title: `Terminal ${++this.titles}`, exited: false, code: null };
    this.bySession.set(sessionId, [...this.list(sessionId), info]);
    this.persist();
    this.changedNow();
    return info;
  }

  /** Attach a view; returns the detach function. A new view replays from offset 0. */
  attach(id: string, sink: TermSink): () => void {
    const view: View = { sink, offset: 0, attaching: false, pending: [] };
    this.views.set(id, view);
    const info = this.get(id);
    if (info?.exited || info?.lost) {
      for (const chunk of this.local.get(id) ?? []) sink.write(chunk);
    } else {
      void this.sync(id, view);
    }
    return () => {
      if (this.views.get(id) === view) this.views.delete(id);
    };
  }

  /** `terminal.attach` from the view's offset; replay or checkpoint, then buffered live output. */
  private async sync(id: string, view: View) {
    if (view.attaching) return;
    view.attaching = true;
    try {
      const r = await this.host.client.terminal.attach({ terminal_id: id, offset: view.offset });
      if (this.views.get(id) !== view) return;
      if (r.truncated) {
        view.sink.reset();
        this.local.delete(id);
        this.localSize.delete(id);
        if (r.checkpoint_b64) this.write(id, view, b64ToBytes(r.checkpoint_b64));
      } else if (view.offset === 0) {
        // A fresh view replays the whole ring; drop the old local copy first.
        this.local.delete(id);
        this.localSize.delete(id);
      }
      this.write(id, view, b64ToBytes(r.replay_b64));
      view.offset = r.end_offset;
    } catch (e) {
      if (errorKind(e) === "not_found") this.markLost(id, "This terminal is no longer running (the engine restarted or it was closed).");
      view.pending = [];
      return;
    } finally {
      view.attaching = false;
    }
    const pending = view.pending;
    view.pending = [];
    for (const o of pending) this.apply(id, view, o);
  }

  /** `terminal.output` for this connection. */
  onOutput(o: TerminalOutput) {
    const view = this.views.get(o.terminal_id);
    if (!view) return;
    if (view.attaching) {
      view.pending.push(o);
      return;
    }
    this.apply(o.terminal_id, view, o);
  }

  private apply(id: string, view: View, o: TerminalOutput) {
    if (o.end <= view.offset) return; // covered by the replay
    if (o.start > view.offset) {
      // A gap (e.g. events dropped across a reconnect): re-read from the ring.
      void this.sync(id, view);
      return;
    }
    const bytes = b64ToBytes(o.data_b64);
    this.write(id, view, bytes.subarray(view.offset - o.start));
    view.offset = o.end;
  }

  private write(id: string, view: View, bytes: Uint8Array) {
    if (!bytes.length) return;
    view.sink.write(bytes);
    const chunks = this.local.get(id) ?? [];
    chunks.push(bytes.slice());
    let size = (this.localSize.get(id) ?? 0) + bytes.length;
    while (size > LOCAL_CAP && chunks.length > 1) size -= chunks.shift()!.length;
    this.local.set(id, chunks);
    this.localSize.set(id, size);
  }

  /** `terminal.exited` from the workspace feed. */
  onExited(e: TerminalExited) {
    const t = this.get(e.terminal_id);
    if (!t) return;
    t.exited = true;
    t.code = e.code ?? null;
    this.replace(t);
  }

  private markLost(id: string, why: string) {
    const t = this.get(id);
    if (!t || t.exited) return;
    t.exited = true;
    t.lost = why;
    this.replace(t);
  }

  private replace(t: TermInfo) {
    this.bySession.set(
      t.sessionId,
      this.list(t.sessionId).map((x) => (x.id === t.id ? { ...t } : x)),
    );
    this.persist();
    this.changedNow();
  }

  /** After a reconnect: every live view re-attaches from its offset. */
  reattachAll() {
    for (const [id, view] of this.views) {
      const t = this.get(id);
      if (t && !t.exited) void this.sync(id, view);
    }
  }

  input(id: string, data: string | Uint8Array) {
    const bytes = typeof data === "string" ? enc.encode(data) : data;
    return this.host.client.terminal.input({ terminal_id: id, data_b64: bytesToB64(bytes) });
  }

  resize(id: string, cols: number, rows: number) {
    const t = this.get(id);
    if (!t || t.exited) return Promise.resolve();
    return this.host.client.terminal.resize({ terminal_id: id, cols: Math.max(2, cols), rows: Math.max(1, rows) }).catch(() => {});
  }

  /** Close (kill-tree) a running terminal, or forget an exited one. */
  async close(id: string) {
    const t = this.get(id);
    if (!t) return;
    if (!t.exited) {
      try {
        await this.host.client.terminal.close({ terminal_id: id });
      } catch (e) {
        if (errorKind(e) !== "not_found") throw e;
      }
    }
    this.views.delete(id);
    this.local.delete(id);
    this.localSize.delete(id);
    this.bySession.set(
      t.sessionId,
      this.list(t.sessionId).filter((x) => x.id !== id),
    );
    this.persist();
    this.changedNow();
  }

  /** A deleted session's terminals are closed by the daemon; forget them here. */
  forgetSession(sessionId: string) {
    for (const t of this.list(sessionId)) {
      this.views.delete(t.id);
      this.local.delete(t.id);
      this.localSize.delete(t.id);
    }
    this.bySession.delete(sessionId);
    this.persist();
    this.changedNow();
  }

  private persist() {
    const out: Record<string, { id: string; title: string }[]> = {};
    for (const [sid, list] of this.bySession) {
      const live = list.filter((t) => !t.exited).map((t) => ({ id: t.id, title: t.title }));
      if (live.length) out[sid] = live;
    }
    this.host.persistTerminals(out);
  }
}
