// Settings driven by `config.schema` (D23, D34): JSON Schema 2020-12 from schemars plus UI
// metadata `{keys: {<dotted key>: {title, scopes_allowed, restart_required, advanced}}}`.
// Pure helpers; the page renders whatever the engine declares.

export type Json = Record<string, unknown>;

export interface KeyMeta {
  key: string;
  title: string;
  scopes: string[];
  restart: boolean;
  advanced: boolean;
}

export type Control =
  | { kind: "boolean" }
  | { kind: "enum"; options: string[] }
  | { kind: "string" }
  | { kind: "number"; integer: boolean; min?: number; max?: number }
  | { kind: "list" }
  | { kind: "json" };

export function uiKeys(ui: unknown): KeyMeta[] {
  const keys = ((ui as Json | undefined)?.keys ?? {}) as Record<string, Json>;
  return Object.entries(keys).map(([key, m]) => ({
    key,
    title: String(m.title ?? key),
    scopes: Array.isArray(m.scopes_allowed) ? (m.scopes_allowed as string[]) : ["user"],
    restart: !!m.restart_required,
    advanced: !!m.advanced,
  }));
}

function deref(root: Json, s: Json | undefined, depth = 0): Json | undefined {
  if (!s || depth > 10) return s;
  const ref = s.$ref;
  if (typeof ref === "string" && ref.startsWith("#/")) {
    let cur: unknown = root;
    for (const seg of ref.slice(2).split("/")) cur = (cur as Json | undefined)?.[seg];
    const { $ref: _r, ...rest } = s;
    return deref(root, { ...(cur as Json), ...rest }, depth + 1);
  }
  // Option<T>: anyOf/oneOf [T, {type: null}] → T.
  for (const k of ["anyOf", "oneOf"] as const) {
    const alts = s[k];
    if (Array.isArray(alts)) {
      const nonNull = (alts as Json[]).filter((a) => a.type !== "null");
      if (nonNull.length === 1) {
        const { [k]: _a, ...rest } = s;
        return deref(root, { ...rest, ...nonNull[0] }, depth + 1);
      }
      if (nonNull.every((a) => "const" in a)) return { ...s, enum: nonNull.map((a) => a.const) };
    }
  }
  return s;
}

/** The sub-schema of a dotted key (`memory.enabled`). */
export function keySchema(schema: unknown, key: string): Json | undefined {
  const root = (schema ?? {}) as Json;
  let cur: Json | undefined = deref(root, root);
  for (const seg of key.split(".")) {
    const props = cur?.properties as Record<string, Json> | undefined;
    cur = deref(root, props?.[seg]);
    if (!cur) return undefined;
  }
  return cur;
}

export function controlFor(s: Json | undefined): Control {
  if (!s) return { kind: "json" };
  if (Array.isArray(s.enum)) return { kind: "enum", options: (s.enum as unknown[]).filter((x) => x !== null).map(String) };
  const types = (Array.isArray(s.type) ? s.type : [s.type]).filter((x) => x !== "null");
  const ty = types[0];
  if (ty === "boolean") return { kind: "boolean" };
  if (ty === "string") return { kind: "string" };
  if (ty === "integer" || ty === "number") return { kind: "number", integer: ty === "integer", min: s.minimum as number | undefined, max: s.maximum as number | undefined };
  if (ty === "array" && ((s.items as Json | undefined)?.type === "string" || s.items === undefined)) return { kind: "list" };
  return { kind: "json" };
}

export function description(s: Json | undefined): string | undefined {
  return typeof s?.description === "string" ? s.description : undefined;
}

export type SectionId = "general" | "appearance" | "genie" | "models" | "providers" | "capabilities" | "connectors" | "permissions" | "usage" | "privacy" | "developer" | "account";

export const SECTIONS: SectionId[] = ["general", "appearance", "genie", "models", "providers", "capabilities", "connectors", "permissions", "usage", "privacy", "developer", "account"];

/** Sections listed in the left column; Account opens from the profile row at its foot. */
export const NAV_SECTIONS: SectionId[] = SECTIONS.filter((s) => s !== "account");

/** Zoom steps for Appearance → Zoom and ⌘= / ⌘-, as in a browser. */
export const ZOOM_STEPS = [0.67, 0.75, 0.8, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2];

/** The next zoom step up (`dir` 1) or down (-1) from `z`. */
export function stepZoom(z: number, dir: 1 | -1): number {
  if (dir > 0) return ZOOM_STEPS.find((s) => s > z + 0.001) ?? ZOOM_STEPS[ZOOM_STEPS.length - 1];
  return [...ZOOM_STEPS].reverse().find((s) => s < z - 0.001) ?? ZOOM_STEPS[0];
}

/** Which Settings section shows a schema key (05 §4 table). */
export function sectionOf(key: string): SectionId {
  const top = key.split(".")[0];
  if (["model", "smallModel", "effort", "hiddenModels"].includes(top)) return "models";
  if (top === "updates") return "general";
  if (top === "theme") return "appearance";
  if (["memory", "chatSearch", "instructions"].includes(top)) return "genie";
  if (["webSearch", "codeExecution", "artifacts", "browser", "imageGeneration"].includes(top)) return "capabilities";
  if (top === "mcpServers") return "connectors";
  if (top === "permissions") return "permissions";
  if (top === "telemetry") return "privacy";
  if (top === "providers") return "providers";
  return "developer";
}

/** Coerce an edited value before `config.set`; `Error` when invalid. */
export function coerce(control: Control, raw: unknown): unknown {
  switch (control.kind) {
    case "boolean":
      return !!raw;
    case "number": {
      const n = typeof raw === "number" ? raw : Number(raw);
      if (!Number.isFinite(n) || (control.integer && !Number.isInteger(n))) throw new Error("Enter a valid number.");
      if (control.min != null && n < control.min) throw new Error(`Must be at least ${control.min}.`);
      if (control.max != null && n > control.max) throw new Error(`Must be at most ${control.max}.`);
      return n;
    }
    case "list":
      return Array.isArray(raw)
        ? raw
        : String(raw)
            .split("\n")
            .map((s) => s.trim())
            .filter(Boolean);
    case "json":
      if (typeof raw !== "string") return raw;
      try {
        return JSON.parse(raw);
      } catch {
        throw new Error("Enter valid JSON.");
      }
    default:
      return raw;
  }
}

/** Search over titles, keys and descriptions (05 §4). */
export function matches(q: string, meta: KeyMeta, s: Json | undefined): boolean {
  const n = q.trim().toLowerCase();
  if (!n) return true;
  return [meta.title, meta.key, description(s) ?? ""].some((x) => x.toLowerCase().includes(n));
}
