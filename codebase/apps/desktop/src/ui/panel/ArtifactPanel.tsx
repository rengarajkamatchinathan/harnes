// Artifact panel (05 §2.5, §8; D28): version switcher, Code/Preview, Copy, Download,
// Fullscreen; HTML runs in <iframe sandbox="allow-scripts"> on the genie-artifact scheme
// (opaque origin), SVG as an image, documents/code rendered in-app; runtime errors
// arrive by postMessage and offer "Fix with Genie".
import { useEffect, useMemo, useRef, useState } from "react";
import type { ArtifactGetResult, ArtifactSummary } from "@genie/protocol";
import { Banner, Button, EmptyState, IconButton, Segmented, Select } from "@genie/ui";
import { t } from "../../i18n";
import { highlight } from "../../markdown/markdown";
import { useApp, useSession } from "../context";
import { MarkdownText } from "../chat/Markdown";

export type ArtifactMessage = { type: "ready" } | { type: "resize"; height: number } | { type: "runtime_error"; message: string; stack: string } | { type: "request_fullscreen" };

/** Validate a postMessage from the sandbox: fixed schema only (05 §8). */
export function parseArtifactMessage(data: unknown): ArtifactMessage | null {
  if (!data || typeof data !== "object") return null;
  const d = data as Record<string, unknown>;
  switch (d.type) {
    case "ready":
    case "request_fullscreen":
      return { type: d.type };
    case "resize":
      return typeof d.height === "number" && Number.isFinite(d.height) ? { type: "resize", height: Math.max(0, Math.min(d.height, 100_000)) } : null;
    case "runtime_error":
      return typeof d.message === "string" ? { type: "runtime_error", message: d.message.slice(0, 4000), stack: typeof d.stack === "string" ? d.stack.slice(0, 8000) : "" } : null;
    default:
      return null;
  }
}

const EXT: Record<string, string> = { document: "md", code: "txt", svg: "svg", mermaid: "mmd", html: "html", react: "jsx" };

export function ArtifactPanel({ sessionId }: { sessionId: string }) {
  const app = useApp();
  const store = useSession(sessionId);
  const [list, setList] = useState<ArtifactSummary[]>([]);
  const [sel, setSel] = useState<string | null>(null);
  const [n, setN] = useState<number | null>(null);
  const [art, setArt] = useState<ArtifactGetResult | null>(null);
  const [view, setView] = useState<"preview" | "code">("preview");
  const [error, setError] = useState<{ message: string; stack: string } | null>(null);
  const [full, setFull] = useState(false);
  const versionsSeen = store?.model.artifacts.length ?? 0;

  useEffect(() => {
    app.client.artifact
      .list({ session_id: sessionId, leaf_id: store?.model.info?.active_leaf_id ?? null })
      .then((r) => {
        setList(r.artifacts);
        const last = r.artifacts[r.artifacts.length - 1];
        if (last) {
          setSel((s) => s ?? last.artifact_id);
          setN(null);
        }
      })
      .catch(() => setList([]));
  }, [app, sessionId, versionsSeen, store?.model.info?.active_leaf_id]);

  const current = list.find((a) => a.artifact_id === sel) ?? null;
  const version = n ?? current?.versions[current.versions.length - 1] ?? null;

  useEffect(() => {
    if (!current || version == null) return;
    setError(null);
    app.client.artifact
      .get({ artifact_id: current.artifact_id, n: version })
      .then(setArt)
      .catch((e) => setError({ message: app.describeError(e), stack: "" }));
  }, [app, current, version]);

  if (!list.length) return <EmptyState icon="artifacts" title={t("artifact.noneTitle")} hint={t("artifact.none")} />;
  const vIdx = current && version != null ? current.versions.indexOf(version) : -1;
  const content = art?.content ?? "";
  const type = art?.type ?? current?.type ?? "code";
  const runsInSandbox = type === "html" || type === "react" || type === "svg";

  const download = async () => {
    try {
      const path = await app.platform.saveFile(`${art?.title ?? "artifact"}.${EXT[type] ?? "txt"}`, { text: content });
      app.toast({ title: t("row.exported", { path }), tone: "success" });
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
    }
  };
  const fix = () => {
    if (!error || !store) return;
    void store.send([{ type: "text", text: t("artifact.fixPrompt", { title: art?.title ?? "", message: error.message, stack: error.stack }) }]);
  };

  return (
    <div className={`artifact ${full ? "artifact--full" : ""}`}>
      <div className="artifact__bar">
        {list.length > 1 ? (
          <Select aria-label="Artifact" value={sel ?? ""} onChange={(v) => (setSel(v), setN(null))} options={list.map((a) => ({ value: a.artifact_id, label: a.title }))} style={{ width: "auto", maxWidth: 200 }} />
        ) : (
          <strong className="artifact__title">{current?.title}</strong>
        )}
        {current && current.versions.length > 1 && (
          <span className="branch" aria-label={t("artifact.version", { n: vIdx + 1, total: current.versions.length })}>
            <IconButton icon="chevronLeft" size="sm" label="Previous version" disabled={vIdx <= 0} onClick={() => setN(current.versions[vIdx - 1])} />v{vIdx + 1}/{current.versions.length}
            <IconButton icon="chevronRight" size="sm" label="Next version" disabled={vIdx >= current.versions.length - 1} onClick={() => setN(current.versions[vIdx + 1])} />
          </span>
        )}
        <span className="grow" />
        <Segmented label="View" value={view} onChange={setView} options={[{ value: "code", label: t("artifact.code") }, { value: "preview", label: t("artifact.preview") }]} />
        <IconButton icon="copy" label={t("chat.copy")} onClick={() => void navigator.clipboard?.writeText(content)} />
        <IconButton icon="download" label={t("artifact.download")} onClick={() => void download()} />
        <IconButton icon="maximize" label={t("artifact.fullscreen")} onClick={() => setFull((f) => !f)} />
      </div>
      {error && (
        <Banner tone="danger" actions={store && <Button size="sm" onClick={fix}>{t("artifact.fix")}</Button>}>
          {t("artifact.error", { message: error.message })}
        </Banner>
      )}
      <div className="artifact__body">
        {view === "code" || !art ? (
          <pre className="code" dangerouslySetInnerHTML={{ __html: highlight(content, type === "html" ? "xml" : type === "react" ? "javascript" : undefined) }} />
        ) : type === "document" ? (
          <MarkdownText text={content} />
        ) : type === "mermaid" ? (
          <MarkdownText text={"```mermaid\n" + content + "\n```"} />
        ) : runsInSandbox && current && version != null ? (
          type === "svg" ? (
            <img className="artifact__svg" alt={art.title} src={app.platform.artifactUrl(current.artifact_id, version)} />
          ) : (
            <Sandbox src={app.platform.artifactUrl(current.artifact_id, version)} title={art.title} onError={setError} onFullscreen={() => setFull(true)} />
          )
        ) : (
          <pre className="code" dangerouslySetInnerHTML={{ __html: highlight(content) }} />
        )}
      </div>
    </div>
  );
}

function Sandbox({ src, title, onError, onFullscreen }: { src: string; title: string; onError: (e: { message: string; stack: string }) => void; onFullscreen: () => void }) {
  const ref = useRef<HTMLIFrameElement>(null);
  const [height, setHeight] = useState(400);
  const handlers = useMemo(() => ({ onError, onFullscreen }), [onError, onFullscreen]);
  useEffect(() => {
    const onMsg = (e: MessageEvent) => {
      if (e.source !== ref.current?.contentWindow) return; // only our iframe
      const m = parseArtifactMessage(e.data);
      if (!m) return;
      if (m.type === "resize") setHeight(Math.max(200, m.height));
      else if (m.type === "runtime_error") handlers.onError({ message: m.message, stack: m.stack });
      else if (m.type === "request_fullscreen") handlers.onFullscreen();
    };
    window.addEventListener("message", onMsg);
    return () => window.removeEventListener("message", onMsg);
  }, [handlers]);
  // No allow-same-origin: opaque origin, no storage, no parent DOM, no IPC (05 §8).
  return <iframe ref={ref} className="artifact__frame" title={title} src={src} sandbox="allow-scripts" referrerPolicy="no-referrer" style={{ height }} />;
}
