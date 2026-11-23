// One-line summaries of tool calls: chat rows (05 §2.2) and Code's compact rows (D54).
import type { IconName } from "@genie/ui";
import type { ToolOutcome } from "@genie/protocol";

export interface ToolSummary {
  icon: IconName;
  title: string;
  /** MCP server, for the server badge. */
  server?: string;
  /** Opens the artifact panel. */
  artifact?: boolean;
  memory?: boolean;
  browser?: boolean;
  /** Generated images to show inline (`image_generate`). */
  images?: GeneratedImage[];
}

export interface GeneratedImage {
  blob_id: string;
  mime: string;
  revised_prompt?: string;
}

/** The images in an `image_generate` result (JSON, possibly as a string). */
export function generatedImages(result: unknown): GeneratedImage[] {
  let v: unknown = result;
  if (typeof v === "string") {
    try {
      v = JSON.parse(v);
    } catch {
      return [];
    }
  }
  const list = v && typeof v === "object" ? (v as { images?: unknown }).images : undefined;
  if (!Array.isArray(list)) return [];
  return list.flatMap((i) => {
    if (!i || typeof i !== "object") return [];
    const { blob_id, mime, revised_prompt } = i as Record<string, unknown>;
    if (typeof blob_id !== "string" || !blob_id || typeof mime !== "string" || !mime.startsWith("image/")) return [];
    return [{ blob_id, mime, ...(typeof revised_prompt === "string" ? { revised_prompt } : {}) }];
  });
}

const str = (v: unknown) => (typeof v === "string" ? v : "");
const short = (s: string, n = 60) => (s.length > n ? `${s.slice(0, n - 1)}…` : s);

function countLines(s: string, prefix: string) {
  return s.split("\n").filter((l) => l.startsWith(prefix) && !l.startsWith(prefix.repeat(3))).length;
}

/** Summarise a call from its name, input and (when known) result text. */
export function summarizeTool(name: string, input: unknown, outcome: ToolOutcome | null, result?: unknown): ToolSummary {
  const inp = (input && typeof input === "object" ? input : {}) as Record<string, unknown>;
  const resText = typeof result === "string" ? result : "";
  if (name.startsWith("mcp__")) {
    const [, server, tool] = name.split("__");
    return { icon: "layers", title: `${server} · ${tool ?? ""}`, server };
  }
  if (name === "web_search") {
    const n = Array.isArray(result) ? result.length : (resText.match(/^\s*\d+\.\s/gm) ?? []).length;
    return { icon: "globe", title: n ? `Searched ${n} sites` : `Searched “${short(str(inp.query))}”` };
  }
  if (name === "web_fetch") return { icon: "globe", title: `Fetched ${short(str(inp.url))}` };
  if (name === "image_generate") {
    const images = generatedImages(result);
    const prompt = short(str(inp.prompt));
    if (outcome === "ok" && images.length) return { icon: "image", title: images.length > 1 ? `Generated ${images.length} images` : "Generated an image", images };
    return { icon: "image", title: prompt ? `Image · ${prompt}` : "Image generation" };
  }
  if (name.startsWith("memory_")) return { icon: "memory", title: "Memory updated", memory: true };
  if (name.startsWith("artifact_")) return { icon: "file", title: str(inp.title) || "Artifact", artifact: true };
  if (name.startsWith("browser_")) return { icon: "globe", title: `Browser · ${name.slice(8).replace(/_/g, " ")}`, browser: true };
  if (name === "preview_start") {
    const n = str(inp.name);
    const cmd = [str(inp.runtimeExecutable) || str(inp.program), ...(Array.isArray(inp.runtimeArgs) ? inp.runtimeArgs.map(str) : [])].filter(Boolean).join(" ");
    return { icon: "play", title: outcome === "error" ? `Couldn't start ${n ? `\`${n}\`` : "the dev server"}` : `${outcome ? "Started" : "Starting"} dev server ${n ? `\`${n}\`` : ""}${cmd ? ` · \`${short(cmd, 50)}\`` : ""}`.trim() };
  }
  if (name === "preview_stop") return { icon: "stop", title: str(inp.name) ? `Stopped dev server \`${str(inp.name)}\`` : "Stopped dev servers" };
  if (name === "preview_logs") return { icon: "terminal", title: `Read dev server logs${str(inp.name) ? ` · \`${str(inp.name)}\`` : ""}` };
  if (name === "shell" || name === "bash") {
    const cmd = short(str(inp.command), 80);
    const failed = outcome === "error";
    const m = /(\d+) failed/.exec(resText);
    return { icon: "terminal", title: `Ran \`${cmd}\`${failed ? (m ? ` · ${m[1]} failed` : " · failed") : ""}` };
  }
  if (["edit", "write", "apply_patch", "multi_edit"].includes(name)) {
    const path = str(inp.path) || str(inp.file_path);
    const diffText = str(inp.diff) || str(inp.patch);
    const add = diffText ? countLines(diffText, "+") : str(inp.new_string || inp.content).split("\n").length;
    const del = diffText ? countLines(diffText, "-") : str(inp.old_string).split("\n").filter(Boolean).length;
    return { icon: "edit", title: `Edited ${path} +${add} −${del}` };
  }
  if (name === "read" || name === "read_file") return { icon: "file", title: `Read ${short(str(inp.path) || str(inp.file_path))}` };
  if (name === "grep" || name === "glob" || name === "list") return { icon: "search", title: `${name} ${short(str(inp.pattern) || str(inp.path))}` };
  if (name === "todo_write") return { icon: "check", title: "Updated the plan" };
  if (name === "task" || name === "subagent") return { icon: "sparkle", title: `Subagent · ${short(str(inp.description))}` };
  return { icon: "tool", title: name };
}

/** Collapse runs of reads into "Read N files" (D54). */
export function groupReads(titles: string[]): string[] {
  const out: string[] = [];
  let reads = 0;
  for (const t of [...titles, ""]) {
    if (t.startsWith("Read ")) reads++;
    else {
      if (reads === 1) out.push("Read 1 file");
      else if (reads > 1) out.push(`Read ${reads} files`);
      reads = 0;
      if (t) out.push(t);
    }
  }
  return out;
}

export function resultText(content: unknown): string {
  if (typeof content === "string") return content;
  if (Array.isArray(content))
    return content
      .map((c) => (typeof c === "string" ? c : c && typeof c === "object" && "text" in c ? String((c as { text: unknown }).text) : JSON.stringify(c)))
      .join("\n");
  return content == null ? "" : JSON.stringify(content, null, 2);
}

/** Head + tail of long output with the count of hidden lines ("show full"). */
export function headTail(text: string, head = 20, tail = 20): { text: string; hidden: number } {
  const lines = text.split("\n");
  if (lines.length <= head + tail + 1) return { text, hidden: 0 };
  return { text: [...lines.slice(0, head), "…", ...lines.slice(-tail)].join("\n"), hidden: lines.length - head - tail };
}

/** Present-tense label for a tool that is running now ("Running a command"), for the turn indicator. */
export function toolActivity(name: string): string {
  if (name.startsWith("mcp__")) return `Using ${name.split("__")[1] ?? "a connector"}`;
  if (name === "shell" || name === "bash") return "Running a command";
  if (["edit", "write", "apply_patch", "multi_edit"].includes(name)) return "Editing files";
  if (name === "read" || name === "read_file") return "Reading files";
  if (name === "grep" || name === "glob" || name === "list") return "Searching the code";
  if (name === "web_search") return "Searching the web";
  if (name === "web_fetch") return "Reading a web page";
  if (name === "image_generate") return "Generating an image";
  if (name.startsWith("browser_")) return "Using the browser";
  if (name === "preview_start") return "Starting a dev server";
  if (name === "preview_stop") return "Stopping a dev server";
  if (name === "preview_logs") return "Reading server logs";
  if (name.startsWith("memory_")) return "Updating memory";
  if (name.startsWith("artifact_")) return "Creating an artifact";
  if (name === "todo_write") return "Planning";
  if (name === "task" || name === "subagent") return "Running a subagent";
  if (/skill/i.test(name)) return "Using a skill";
  return `Using ${name.replace(/_/g, " ")}`;
}

/** "Read 4 files, searched the code 2 times, ran a command" for a folded group of tool calls. */
export function toolGroupSummary(names: string[]): string {
  const counts = new Map<string, number>();
  const kind = (n: string) =>
    n === "read" || n === "read_file" ? "read" : n === "grep" || n === "glob" || n === "list" ? "search" : n === "shell" || n === "bash" ? "run" : ["edit", "write", "apply_patch", "multi_edit"].includes(n) ? "edit" : n === "web_search" ? "web" : n === "web_fetch" ? "fetch" : n.startsWith("browser_") ? "browse" : "other";
  for (const n of names) counts.set(kind(n), (counts.get(kind(n)) ?? 0) + 1);
  const plural = (k: number, one: string, many: string) => (k === 1 ? one : many.replace("#", String(k)));
  const phrase: Record<string, (k: number) => string> = {
    read: (k) => plural(k, "read a file", "read # files"),
    search: (k) => plural(k, "searched the code", "searched the code # times"),
    run: (k) => plural(k, "ran a command", "ran # commands"),
    edit: (k) => plural(k, "edited a file", "edited # files"),
    web: (k) => plural(k, "searched the web", "searched the web # times"),
    fetch: (k) => plural(k, "read a web page", "read # web pages"),
    browse: (k) => plural(k, "used the browser", "used the browser # times"),
    other: (k) => plural(k, "used a tool", "used # tools"),
  };
  const out = [...counts.entries()].map(([k, n]) => phrase[k](n)).join(", ");
  return out ? out[0].toUpperCase() + out.slice(1) : "";
}
