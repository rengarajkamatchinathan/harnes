// Streaming Markdown renderer (05 §7, D57), from prototype 37: an incremental block
// splitter; settled blocks are parsed once and memoised, only the unstable tail re-renders.
// highlight.js while streaming; KaTeX and Mermaid only for settled blocks, off the
// streaming path (Mermaid is lazy-loaded and pre-warmed on idle).
//
// Model output is untrusted: raw HTML is escaped, links get `data-href` and open in the
// system browser via the Platform adapter, remote images are not loaded (CSP + link).
import { Marked, type TokenizerAndRendererExtension, type Tokens } from "marked";
import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import css from "highlight.js/lib/languages/css";
import go from "highlight.js/lib/languages/go";
import java from "highlight.js/lib/languages/java";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import python from "highlight.js/lib/languages/python";
import rust from "highlight.js/lib/languages/rust";
import sql from "highlight.js/lib/languages/sql";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";
import diff from "highlight.js/lib/languages/diff";
import powershell from "highlight.js/lib/languages/powershell";

for (const [name, lang] of Object.entries({ bash, css, go, java, javascript, json, python, rust, sql, typescript, xml, yaml, diff, powershell })) hljs.registerLanguage(name, lang);
hljs.registerAliases(["sh", "shell", "zsh"], { languageName: "bash" });
hljs.registerAliases(["ts", "tsx"], { languageName: "typescript" });
hljs.registerAliases(["js", "jsx"], { languageName: "javascript" });
hljs.registerAliases(["html", "svg"], { languageName: "xml" });
hljs.registerAliases(["py"], { languageName: "python" });
hljs.registerAliases(["rs"], { languageName: "rust" });
hljs.registerAliases(["yml"], { languageName: "yaml" });
hljs.registerAliases(["ps1", "pwsh"], { languageName: "powershell" });
export { hljs };

export const escapeHtml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

export function highlight(code: string, lang?: string | null): string {
  const l = lang && hljs.getLanguage(lang) ? lang : null;
  return l ? hljs.highlight(code, { language: l }).value : escapeHtml(code);
}

// KaTeX is loaded on idle (05 §7); until then math renders as source.
type Katex = { renderToString: (tex: string, o: { displayMode?: boolean; throwOnError?: boolean }) => string };
let katex: Katex | null = null;
let katexP: Promise<void> | null = null;
export function preloadKatex(): Promise<void> {
  katexP ??= import("katex").then((m) => {
    katex = (m.default ?? m) as Katex;
  });
  return katexP;
}
const tex = (src: string, display: boolean) =>
  katex ? katex.renderToString(src, { displayMode: display, throwOnError: false }) : `<code class="math-src">${escapeHtml(display ? `$$${src}$$` : `$${src}$`)}</code>`;

const mathBlock: TokenizerAndRendererExtension = {
  name: "mathBlock",
  level: "block",
  start: (src) => src.indexOf("$$"),
  tokenizer(src) {
    const m = /^\$\$\n?([\s\S]+?)\n?\$\$(?:\n|$)/.exec(src);
    if (m) return { type: "mathBlock", raw: m[0], text: m[1] };
  },
  renderer: (t) => tex(t.text as string, true),
};
const mathInline: TokenizerAndRendererExtension = {
  name: "mathInline",
  level: "inline",
  start: (src) => src.indexOf("$"),
  tokenizer(src) {
    const m = /^\$([^\s$](?:[^$\n]*?[^\s$])?)\$/.exec(src);
    if (m) return { type: "mathInline", raw: m[0], text: m[1] };
  },
  renderer: (t) => tex(t.text as string, false),
};
/** Citation marker inserted by `withCitations`: `⟦n⟧`. */
const citation: TokenizerAndRendererExtension = {
  name: "citation",
  level: "inline",
  start: (src) => src.indexOf("⟦"),
  tokenizer(src) {
    const m = /^⟦(\d+)⟧/.exec(src);
    if (m) return { type: "citation", raw: m[0], text: m[1] };
  },
  renderer: (t) => `<sup class="cite" data-cite="${t.text}" tabindex="0" role="button" aria-label="Source ${t.text}">[${t.text}]</sup>`,
};

const safeHref = (href: string) => /^(https?:|mailto:)/i.test(href);

function make(settled: boolean) {
  const m = new Marked({ gfm: true });
  m.use({ extensions: settled ? [mathBlock, mathInline, citation] : [citation] });
  m.use({
    renderer: {
      html: (t: Tokens.HTML | Tokens.Tag) => escapeHtml(t.text),
      link({ href, tokens }: Tokens.Link) {
        const inner = this.parser.parseInline(tokens);
        return safeHref(href) ? `<a href="#" data-href="${escapeHtml(href)}" title="${escapeHtml(href)}">${inner}</a>` : inner;
      },
      image: ({ href, text }: Tokens.Image) => (safeHref(href) ? `<a href="#" data-href="${escapeHtml(href)}">${escapeHtml(text || href)}</a>` : escapeHtml(text)),
      code({ text, lang }: Tokens.Code) {
        const l = (lang ?? "").trim().split(/\s+/)[0];
        // Structured questions are answered in the stepper under the reply, not shown as JSON.
        if (l === "questions") return "";
        if (l === "mermaid" && settled) return `<div class="mermaid-block" data-mermaid="${encodeURIComponent(text)}"><pre><code>${escapeHtml(text)}</code></pre></div>`;
        return `<div class="code-block"><div class="code-block__bar"><span>${escapeHtml(l)}</span><button type="button" class="code-copy" data-copy="${encodeURIComponent(text)}">Copy</button></div><pre><code class="hljs">${highlight(text, l)}</code></pre></div>`;
      },
    },
  });
  return m;
}
const mdSettled = make(true);
const mdStream = make(false);

// Settled blocks are immutable: cache their HTML so remounts (virtualisation) never re-parse.
const cache = new Map<string, string>();
let cacheKatex = false;
export function renderSettled(src: string): string {
  if (cacheKatex !== !!katex) {
    cache.clear();
    cacheKatex = !!katex;
  }
  let h = cache.get(src);
  if (h === undefined) {
    h = mdSettled.parse(src) as string;
    if (cache.size > 5000) cache.clear();
    cache.set(src, h);
  }
  return h;
}
export const renderTail = (src: string) => mdStream.parse(completeTableHead(src)) as string;

/**
 * A streaming table's header row arrives before its `|---|` line, so it showed as raw
 * `| Step | Cost |` text until then. Supply a provisional separator (pure, for tests).
 */
export function completeTableHead(src: string): string {
  const lines = src.split("\n");
  const head = lines[0]?.trim() ?? "";
  if (!head.startsWith("|") || head.length < 3) return src;
  const sep = lines[1];
  const sepLike = sep !== undefined && /^\s*\|?[\s:|-]*$/.test(sep);
  // The separator is complete once another line follows it; until then it may be half-typed.
  if (sepLike && lines.length > 2) return src;
  if (sep !== undefined && !sepLike) return src; // not a table after all
  const cols = Math.max(1, head.replace(/^\||\|$/g, "").split("|").length);
  return [lines[0], "|" + " --- |".repeat(cols)].join("\n");
}

/** Incremental splitter: a blank line outside a fence / `$$` block ends a block. */
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
      const t = this.text.slice(this.scan, nl).trim();
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

const splitCache = new Map<string, string[]>();
export function splitAll(text: string): string[] {
  const hit = splitCache.get(text);
  if (hit) return hit;
  const s = new BlockSplitter();
  s.push(text + "\n");
  s.finish();
  if (splitCache.size > 2000) splitCache.clear();
  splitCache.set(text, s.settled);
  return s.settled;
}

/** Insert `⟦n⟧` markers at citation end offsets (D36). Offsets are UTF-16 indices. */
export function withCitations(text: string, cites: { end: number }[]): string {
  if (!cites.length) return text;
  const order = cites.map((c, i) => ({ end: c.end, n: i + 1 })).sort((a, b) => b.end - a.end);
  let out = text;
  for (const { end, n } of order) {
    const at = Math.max(0, Math.min(out.length, end));
    out = `${out.slice(0, at)}⟦${n}⟧${out.slice(at)}`;
  }
  return out;
}

// Mermaid: lazy-loaded (pre-warmed on idle), renders serialised, results cached by source;
// the last good render stays while a new one is pending (05 §7).
type Mermaid = { initialize: (o: object) => void; render: (id: string, src: string) => Promise<{ svg: string }> };
const svgCache = new Map<string, string>();
let mermaidP: Promise<Mermaid> | null = null;
let theme = "default";
let chain: Promise<unknown> = Promise.resolve();
let seq = 0;
export function preloadMermaid(dark: boolean): Promise<Mermaid> {
  theme = dark ? "dark" : "default";
  mermaidP ??= import("mermaid").then((m) => {
    const mm = m.default as unknown as Mermaid;
    mm.initialize({ startOnLoad: false, theme, securityLevel: "strict" });
    return mm;
  });
  return mermaidP;
}
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
      const mermaid = await preloadMermaid(theme === "dark");
      try {
        const { svg } = await mermaid.render(`mmd-${++seq}`, src);
        svgCache.set(src, svg);
        node.innerHTML = svg;
      } catch (e) {
        node.insertAdjacentHTML("beforeend", `<div class="mermaid-error">${escapeHtml(String(e))}</div>`);
      }
    });
  });
}

/** Pre-load Mermaid and KaTeX at startup on an idle callback (05 §7). */
export function preloadOnIdle(dark: boolean) {
  const idle = (globalThis as { requestIdleCallback?: (f: () => void) => void }).requestIdleCallback ?? ((f: () => void) => setTimeout(f, 500));
  idle(() => {
    void preloadKatex();
    void preloadMermaid(dark);
  });
}
