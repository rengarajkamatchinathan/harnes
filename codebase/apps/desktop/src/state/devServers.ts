// Dev servers (ticket 47 phase 2; 02 §6.6 `preview.*`): the client side of the daemon's
// `.genie/launch.json` servers, for the Browser tab's Dev servers menu and chat server cards.
//
// - `preview.list` loads a session's servers and `launch.json` entries on first use.
// - `preview.changed` (workspace feed) replaces a session's server list; `onChanged` returns
//   the servers that just came up so the app can open them in the Browser tab.
import type { GenieClient, PreviewChanged, PreviewListResult, PreviewServer } from "@genie/protocol";
import { Emitter } from "./emitter";

const EMPTY: PreviewListResult = { servers: [], configs: [], auto_verify: true };

export class DevServers extends Emitter {
  private bySession = new Map<string, PreviewListResult>();
  private loading = new Set<string>();

  constructor(private client: GenieClient) {
    super();
  }

  get(sessionId: string): PreviewListResult {
    return this.bySession.get(sessionId) ?? EMPTY;
  }

  server(sessionId: string, name: string): PreviewServer | undefined {
    return this.get(sessionId).servers.find((s) => s.name === name);
  }

  /** Load once per session (call from render effects). */
  ensure(sessionId: string) {
    if (!this.bySession.has(sessionId)) void this.refresh(sessionId);
  }

  async refresh(sessionId: string) {
    if (this.loading.has(sessionId)) return;
    this.loading.add(sessionId);
    try {
      this.bySession.set(sessionId, await this.client.preview.list({ session_id: sessionId }));
      this.changed();
    } catch {
      /* not a Code session, or an older daemon: no servers */
    } finally {
      this.loading.delete(sessionId);
    }
  }

  /** Sessions already loaded (re-listed after a reconnect). */
  refreshAll() {
    for (const sid of this.bySession.keys()) void this.refresh(sid);
  }

  /** `preview.changed`; returns servers that turned `running` with this change. */
  onChanged(p: PreviewChanged): PreviewServer[] {
    const before = this.bySession.get(p.session_id);
    const wasUp = new Set((before?.servers ?? []).filter((s) => s.state === "running").map((s) => `${s.name}\u0000${s.pid ?? ""}`));
    // A new entry may have been saved to launch.json too: re-list it in the background.
    if (p.servers.some((s) => !before?.configs.includes(s.name))) void this.refresh(p.session_id);
    this.bySession.set(p.session_id, { ...(before ?? EMPTY), servers: p.servers });
    this.changed();
    return p.servers.filter((s) => s.state === "running" && !wasUp.has(`${s.name}\u0000${s.pid ?? ""}`));
  }

  start(sessionId: string, name: string): Promise<PreviewServer> {
    return this.client.preview.start({ session_id: sessionId, name });
  }

  async stop(sessionId: string, name?: string) {
    await this.client.preview.stop({ session_id: sessionId, ...(name ? { name } : {}) });
  }

  logs(sessionId: string, name: string, lines = 200): Promise<string> {
    return this.client.preview.logs({ session_id: sessionId, name, lines }).then((r) => r.text);
  }
}
