// Markdown pipeline: block splitter + two marked instances (settled vs streaming tail).
import { Marked, type TokenizerAndRendererExtension } from "marked";
import hljs from "highlight.js/lib/core";
import rust from "highlight.js/lib/languages/rust";
import typescript from "highlight.js/lib/languages/typescript";
import python from "highlight.js/lib/languages/python";
import go from "highlight.js/lib/languages/go";
import sql from "highlight.js/lib/languages/sql";
import bash from "highlight.js/lib/languages/bash";
import json from "highlight.js/lib/languages/json";
import katex from "katex";

hljs.registerLanguage("rust", rust);
hljs.registerLanguage("typescript", typescript);
hljs.registerLanguage("python", python);
hljs.registerLanguage("go", go);
hljs.registerLanguage("sql", sql);
hljs.registerLanguage("bash", bash);
hljs.registerLanguage("json", json);
export { hljs };

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

const mathBlock: TokenizerAndRendererExtension = {
  name: "mathBlock",
  level: "block",
  start: (src) => src.indexOf("$$"),
  tokenizer(src) {
    const m = /^\$\$\n?([\s\S]+?)\n?\$\$(?:\n|$)/.exec(src);
    if (m) return { type: "mathBlock", raw: m[0], text: m[1] };
  },
  renderer: (t) => katex.renderToString(t.text as string, { displayMode: true, throwOnError: false }),
};
const mathInline: TokenizerAndRendererExtension = {
  name: "mathInline",
  level: "inline",
  start: (src) => src.indexOf("$"),
  tokenizer(src) {
    const m = /^\$([^\s$](?:[^$\n]*?[^\s$])?)\$/.exec(src);
    if (m) return { type: "mathInline", raw: m[0], text: m[1] };
  },
  renderer: (t) => katex.renderToString(t.text as string, { throwOnError: false }),
};

function make(settled: boolean) {
  const m = new Marked({ gfm: true });
  if (settled) m.use({ extensions: [mathBlock, mathInline] });
  m.use({
    renderer: {
      code({ text, lang }) {
        if (lang === "mermaid" && settled) return `<div class="mermaid-block" data-mermaid="${encodeURIComponent(text)}"></div>`;
        const l = lang && hljs.getLanguage(lang) ? lang : null;
        const html = l ? hljs.highlight(text, { language: l }).value : esc(text);
        return `<pre><code class="hljs">${html}</code></pre>`;
      },
    },
  });
  return m;
}
const mdSettled = make(true);
const mdStream = make(false);

// Settled blocks are immutable: cache their HTML so remounts (virtualisation) never re-parse.
const cache = new Map<string, string>();
export function renderSettled(src: string): string {
  let h = cache.get(src);
  if (h === undefined) {
    h = mdSettled.parse(src) as string;
    if (cache.size > 5000) cache.clear();
    cache.set(src, h);
  }
  return h;
}
export const renderTail = (src: string) => mdStream.parse(src) as string;

/** Incremental splitter: blank line outside a fence / $$ block ends a block. */
export class BlockSplitter {
  settled: string[] = [];
  text = "";
  private scan = 0;
  private blockStart = 0;
  private fence: string | null = null;
  private inMath = false;

  push(chunk: string) {
    this.text += chunk;
    let nl: number;
    while ((nl = this.text.indexOf("\n", this.scan)) >= 0) {
      const line = this.text.slice(this.scan, nl);
      const t = line.trim();
      if (this.fence) {
        if (t.startsWith(this.fence) && t.replace(/[`~]/g, "") === "") this.fence = null;
      } else if (/^(```|~~~)/.test(t)) {
        this.fence = t.slice(0, 3);
      } else if (t === "$$") {
        this.inMath = !this.inMath;
      } else if (t === "" && !this.inMath) {
        const block = this.text.slice(this.blockStart, this.scan).trim();
        if (block) this.settled.push(block);
        this.blockStart = nl + 1;
      }
      this.scan = nl + 1;
    }
  }
  tail() {
    return this.text.slice(this.blockStart);
  }
  finish() {
    const b = this.tail().trim();
    if (b) this.settled.push(b);
    this.blockStart = this.text.length;
    this.scan = this.text.length;
  }
}

export function splitAll(text: string): string[] {
  const s = new BlockSplitter();
  s.push(text + "\n");
  s.finish();
  return s.settled;
}

// Mermaid: lazy-loaded, renders serialised, results cached by source.
const svgCache = new Map<string, string>();
let mermaidP: Promise<typeof import("mermaid").default> | null = null;
let chain: Promise<unknown> = Promise.resolve();
let seq = 0;
export function renderMermaidIn(el: HTMLElement | null) {
  if (!el) return;
  el.querySelectorAll<HTMLElement>(".mermaid-block:not([data-done])").forEach((node) => {
    node.dataset.done = "1";
    const src = decodeURIComponent(node.dataset.mermaid || "");
    const hit = svgCache.get(src);
    if (hit) {
      node.innerHTML = hit;
      return;
    }
    chain = chain.then(async () => {
      if (!mermaidP)
        mermaidP = import("mermaid").then((m) => {
          m.default.initialize({ startOnLoad: false, theme: "default", securityLevel: "strict" });
          return m.default;
        });
      const mermaid = await mermaidP;
      try {
        const { svg } = await mermaid.render(`mmd-${++seq}`, src);
        svgCache.set(src, svg);
        node.innerHTML = svg;
      } catch (e) {
        node.textContent = "mermaid error: " + e;
      }
    });
  });
}
