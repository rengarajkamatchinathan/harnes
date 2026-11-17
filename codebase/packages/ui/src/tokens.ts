// Token names from 05 §6 and an OKLCH → sRGB contrast helper used by the WCAG AA test.

export const COLOR_TOKENS = [
  "--bg",
  "--bg-subtle",
  "--surface",
  "--surface-raised",
  "--border",
  "--text",
  "--text-muted",
  "--accent",
  "--accent-fg",
  "--focus-ring",
  "--danger",
  "--warning",
  "--success",
  "--info",
  "--diff-add",
  "--diff-del",
  "--code-bg",
] as const;

export const TYPE_TOKENS = ["--font-ui", "--font-mono", "--text-xs", "--text-sm", "--text-base", "--text-md", "--text-lg", "--text-xl", "--text-2xl", "--chat-font-size"] as const;
export const SHAPE_TOKENS = ["--radius-sm", "--radius-md", "--radius-lg"] as const;
export const MOTION_TOKENS = ["--dur-fast", "--dur-base"] as const;

export type Oklch = { l: number; c: number; h: number };

export function parseOklch(s: string): Oklch | null {
  const m = /oklch\(\s*([\d.]+)\s+([\d.]+)\s+([\d.]+)/.exec(s);
  return m ? { l: +m[1], c: +m[2], h: +m[3] } : null;
}

/** OKLCH → linear sRGB (Björn Ottosson's OKLab matrices), clipped to gamut. */
export function oklchToLinearSrgb({ l, c, h }: Oklch): [number, number, number] {
  const a = c * Math.cos((h * Math.PI) / 180);
  const b = c * Math.sin((h * Math.PI) / 180);
  const l_ = l + 0.3963377774 * a + 0.2158037573 * b;
  const m_ = l - 0.1055613458 * a - 0.0638541728 * b;
  const s_ = l - 0.0894841775 * a - 1.291485548 * b;
  const L = l_ ** 3;
  const M = m_ ** 3;
  const S = s_ ** 3;
  const clip = (x: number) => Math.max(0, Math.min(1, x));
  return [
    clip(4.0767416621 * L - 3.3077115913 * M + 0.2309699292 * S),
    clip(-1.2684380046 * L + 2.6097574011 * M - 0.3413193965 * S),
    clip(-0.0041960863 * L - 0.7034186147 * M + 1.707614701 * S),
  ];
}

export function luminance(c: Oklch): number {
  const [r, g, b] = oklchToLinearSrgb(c);
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** WCAG 2 contrast ratio. */
export function contrast(a: Oklch, b: Oklch): number {
  const la = luminance(a);
  const lb = luminance(b);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}
