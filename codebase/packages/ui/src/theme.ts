// Theme, density and font settings (05 §4 Appearance, §6): follow the system with a manual
// override; everything is CSS variables on <html>.

export type ThemePref = "system" | "light" | "dark";
export type Density = "comfortable" | "compact";
export type ChatFont = "default" | "system" | "dyslexia";

export interface Appearance {
  theme: ThemePref;
  density: Density;
  chatFont: ChatFont;
  /** Chat body size in px (Appearance); the UI scale stays on the token scale. */
  fontSize: number;
}

export const DEFAULT_APPEARANCE: Appearance = { theme: "system", density: "comfortable", chatFont: "default", fontSize: 15 };

export function resolveTheme(pref: ThemePref, systemDark: boolean): "light" | "dark" {
  return pref === "system" ? (systemDark ? "dark" : "light") : pref;
}

const CHAT_FONTS: Record<ChatFont, string> = {
  default: "var(--font-ui)",
  system: "system-ui, -apple-system, 'Segoe UI', sans-serif",
  // Bundled dyslexia-friendly font is a v1 follow-up; fall back to wide system fonts.
  dyslexia: "'OpenDyslexic', 'Comic Sans MS', 'Verdana', sans-serif",
};

/** Apply to `root`; returns a disposer that stops following the system theme. */
export function applyAppearance(a: Appearance, root: HTMLElement = document.documentElement): () => void {
  const mq = typeof window !== "undefined" && window.matchMedia ? window.matchMedia("(prefers-color-scheme: dark)") : null;
  const set = () => {
    root.dataset.theme = resolveTheme(a.theme, mq?.matches ?? false);
  };
  set();
  root.dataset.density = a.density;
  root.style.setProperty("--chat-font-size", `${Math.max(11, Math.min(30, a.fontSize))}px`);
  root.style.setProperty("--chat-font", CHAT_FONTS[a.chatFont]);
  if (a.theme !== "system" || !mq) return () => {};
  mq.addEventListener("change", set);
  return () => mq.removeEventListener("change", set);
}
