// The Platform adapter (01 §1.3, §2.1 rule 8; D8 condition 2): the only way the React app
// reaches the host. `platform/tauri/` implements it with Tauri IPC; `platform/mock/` is an
// in-memory engine for tests and for running the UI in a plain browser.
//
// Message shapes mirror `apps/desktop/src-tauri/src/relay.rs` (`HostMessage`, `HostStatus`,
// `HostInfo`, `WireError`).

export type ConnState = "connecting" | "starting" | "restarting" | "ready" | "degraded" | "busy_updating" | "reconnecting" | "failed";

export interface HostStatus {
  state: ConnState;
  detail?: string;
  engine_version?: string;
  /** The secrets store is locked; `secrets.unlock` with the passphrase (D26). */
  locked?: string;
  /** The engine can't open its database: nothing is being saved (04 §2). */
  storage?: StorageProblem;
  running?: number;
}

/** Mirrors `genie_protocol::handshake::StorageProblem`. */
export interface StorageProblem {
  /** `key_missing` | `open_failed` */
  reason: string;
  detail: string;
  recovery_available?: boolean;
}

/** A daemon notification as relayed: `{method, params}`. */
export interface RelayedNotification {
  method: string;
  params: unknown;
}

export type HostMessage =
  | ({ kind: "status" } & HostStatus)
  | { kind: "batch"; messages: RelayedNotification[] }
  | { kind: "server_request"; req_id: number; method: string; params: unknown }
  | { kind: "request_cancelled"; req_id: number }
  | { kind: "reload"; session_id: string };

export interface HostInfo {
  client_instance_id: string;
  os: string;
  app_version: string;
  batch_ms: number;
  /** OS account's full name; fallback for the display name. */
  user_name?: string;
}

export interface CliInstall {
  path: string;
  target: string;
  dir: string;
  on_path: boolean;
}

export interface WireError {
  code: number;
  message: string;
  data?: { kind?: string; [k: string]: unknown } | null;
}

export interface Platform {
  readonly kind: "tauri" | "mock";
  /** Start receiving host messages (status first). Call again after nothing; idempotent per page. */
  connect(onMessage: (m: HostMessage) => void): Promise<HostInfo>;
  /** One JSON-RPC request to the daemon; rejects with a `WireError`. */
  request(method: string, params: unknown): Promise<unknown>;
  /** Answer a `server_request` (e.g. `permission.ask`). */
  respond(reqId: number, result: { ok: unknown } | { error: WireError }): Promise<void>;
  /** "Retry" on the engine error card. */
  retry(): Promise<void>;
  /** System browser for http(s)/mailto links (citations, sign-in pages). */
  openExternal(url: string): Promise<void>;
  /** Open a local file or folder with the OS ("open in editor"). */
  openPath(path: string): Promise<void>;
  /** `state.json`: window/panel state, never settings (D23). */
  loadState(): Promise<Record<string, unknown>>;
  saveState(state: Record<string, unknown>): Promise<void>;
  /** Save an export to Downloads; resolves with the saved path. */
  saveFile(name: string, content: { text: string } | { base64: string }): Promise<string>;
  /** Native folder picker (Code: new session); null when cancelled. */
  pickFolder(title?: string): Promise<string | null>;
  /** Native file picker (a page's file input during browser take-over); null when cancelled. */
  pickFiles(opts: { multiple: boolean; title?: string }): Promise<string[] | null>;
  /** Native save dialog; resolves with the chosen path or null. */
  pickSavePath(defaultName: string, filters?: { name: string; extensions: string[] }[]): Promise<string | null>;
  /** OS notification (paused asks, workspace `notification`) when the window is in the background. */
  notify(title: string, body?: string): Promise<void>;
  /** "Install `genie` command in PATH" (D40). */
  installCli(): Promise<CliInstall>;
  /** Third-party licence notices shipped with the app; null when not bundled. */
  notices(): Promise<string | null>;
  /** URL of an artifact page on the sandboxed scheme (05 §8). */
  artifactUrl(artifactId: string, n: number): string;
  log(level: "info" | "warn" | "error", message: string): void;
}

export function isWireError(e: unknown): e is WireError {
  return typeof e === "object" && e !== null && "code" in e && "message" in e;
}

export function errorKind(e: unknown): string | undefined {
  return isWireError(e) ? (e.data?.kind as string | undefined) : undefined;
}

export function errorMessage(e: unknown): string {
  if (isWireError(e)) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}
