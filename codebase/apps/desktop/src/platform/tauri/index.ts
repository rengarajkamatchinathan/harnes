// Tauri implementation of the Platform adapter. This folder is the ONLY place that may
// import `@tauri-apps/*` (01 §1.3; enforced by test/architecture.test.ts).
import { Channel, invoke } from "@tauri-apps/api/core";
import type { HostInfo, HostMessage, Platform, WireError } from "../types";

export function createTauriPlatform(): Platform {
  let os = "";
  return {
    kind: "tauri",
    async connect(onMessage) {
      const ch = new Channel<HostMessage>();
      ch.onmessage = onMessage;
      const info = await invoke<HostInfo>("host_connect", { onMessage: ch });
      os = info.os;
      return info;
    },
    request: (method, params) => invoke("host_request", { method, params: params ?? {} }),
    async respond(reqId, result) {
      const args = "ok" in result ? { reqId, result: result.ok ?? null, error: null } : { reqId, result: null, error: result.error as WireError };
      await invoke("host_respond", args);
    },
    retry: () => invoke("host_retry"),
    openExternal: (url) => invoke("host_open_external", { url }),
    openPath: (path) => invoke("host_open_path", { path }),
    loadState: () => invoke<Record<string, unknown>>("host_state_load"),
    saveState: (state) => invoke("host_state_save", { state }),
    saveFile: (name, content) => invoke<string>("host_save_file", { name, text: "text" in content ? content.text : null, base64: "base64" in content ? content.base64 : null }),
    artifactUrl(artifactId, n) {
      const path = `${encodeURIComponent(artifactId)}/${n}/`;
      // WebView2 serves custom schemes as http://<scheme>.localhost (05 §8).
      return os === "windows" ? `http://genie-artifact.localhost/${path}` : `genie-artifact://localhost/${path}`;
    },
    log(level, message) {
      void invoke("host_log", { level, message }).catch(() => {});
    },
  };
}
