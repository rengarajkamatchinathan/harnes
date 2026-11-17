import { memo, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useVirtualizer } from "@tanstack/react-virtual";
import { renderMermaidIn, renderSettled, renderTail, splitAll } from "./markdown";
import { connect, engine, startLoop, type Msg } from "./engine";
import { runBench } from "./bench";

const Block = memo(function Block({ src, settled }: { src: string; settled: boolean }) {
  const ref = useRef<HTMLDivElement>(null);
  const html = settled ? renderSettled(src) : renderTail(src);
  useEffect(() => {
    if (settled) renderMermaidIn(ref.current); // Mermaid only once the block has settled
  }, [html, settled]);
  return <div ref={ref} className="md" dangerouslySetInnerHTML={{ __html: html }} />;
});

const splitCache = new WeakMap<Msg, string[]>();
const Message = memo(function Message({ msg }: { msg: Msg }) {
  if (msg.role === "user") return <div className="ml-auto max-w-[70%] rounded-xl bg-indigo-900/60 px-4 py-2 whitespace-pre-wrap">{msg.text}</div>;
  let blocks = splitCache.get(msg);
  if (!blocks) splitCache.set(msg, (blocks = splitAll(msg.text)));
  return (
    <div>
      {blocks.map((b, i) => (
        <Block key={i} src={b} settled />
      ))}
    </div>
  );
});

function Streaming() {
  const s = engine.splitter;
  if (!s) return null;
  return (
    <div>
      {s.settled.map((b, i) => (
        <Block key={i} src={b} settled />
      ))}
      <Block key="tail" src={s.tail()} settled={false} />
      <span className="inline-block h-4 w-2 animate-pulse bg-indigo-400 align-middle" />
    </div>
  );
}

function Transcript() {
  const parentRef = useRef<HTMLDivElement>(null);
  const items = engine.messages;
  const streaming = !!engine.splitter;
  const count = items.length + (streaming ? 1 : 0);
  const virt = useVirtualizer({ count, getScrollElement: () => parentRef.current, estimateSize: () => 900, overscan: 2 });
  useEffect(() => {
    engine.scrollEl = parentRef.current;
  }, []);
  return (
    <div ref={parentRef} className="flex-1 overflow-auto" onWheel={() => (engine.stick = false)}>
      <div style={{ height: virt.getTotalSize(), position: "relative" }}>
        {virt.getVirtualItems().map((vi) => (
          <div
            key={vi.key}
            data-index={vi.index}
            ref={virt.measureElement}
            className="absolute left-0 top-0 w-full px-8 py-3"
            style={{ transform: `translateY(${vi.start}px)` }}
          >
            {vi.index < items.length ? <Message msg={items[vi.index]} /> : <Streaming />}
          </div>
        ))}
      </div>
    </div>
  );
}

const color: Record<string, string> = { connected: "bg-emerald-500", connecting: "bg-amber-500", reconnecting: "bg-red-500", spawning: "bg-amber-500" };

export function App() {
  const [, setV] = useState(0);
  const [phase, setPhase] = useState("");
  engine.rerender = () => setV((x) => x + 1);
  useEffect(() => {
    startLoop();
    connect().then((info) => {
      if (info.bench) runBench(setPhase);
    });
  }, []);
  const st = engine.status;
  const start = (sc: string) => {
    engine.reset(sc === "long" ? null : `Run the ${sc} scenario`);
    invoke("chat_start", { scenario: sc }).catch((e) => alert(e));
  };
  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-3 border-b border-white/10 bg-black/40 px-4 py-2 text-sm">
        <span className={`h-2.5 w-2.5 rounded-full ${color[st?.state ?? ""] ?? "bg-gray-500"}`} />
        <span>
          daemon: <b>{st?.state ?? "…"}</b> {st?.detail}
          {st?.reconnect_ms != null && <> · reconnected in {st.reconnect_ms.toFixed(0)} ms</>}
        </span>
        <span className="ml-auto text-gray-400">{phase}</span>
        {["normal", "burst", "long"].map((s) => (
          <button key={s} className="rounded bg-white/10 px-2 py-1 hover:bg-white/20" onClick={() => start(s)}>
            {s}
          </button>
        ))}
        <button className="rounded bg-red-900/60 px-2 py-1 hover:bg-red-800" onClick={() => invoke("kill_daemon").catch((e) => alert(e))}>
          kill daemon
        </button>
        <button className="rounded bg-white/10 px-2 py-1 hover:bg-white/20" onClick={() => ((engine.stick = true), (engine.dirty = true))}>
          ↓
        </button>
      </div>
      <Transcript />
    </div>
  );
}
