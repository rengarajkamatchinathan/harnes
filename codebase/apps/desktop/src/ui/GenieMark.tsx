// Genie brand mark ("Magic Braces"): two curly braces around a four-point star. Braces take
// currentColor, the star takes --genie-star. `idle` breathes slowly, `thinking` spins the star
// while work runs, `still` holds. Small sizes switch to heavier strokes so it stays legible.
// Source SVGs and the app-icon masters live in apps/desktop/brand/.

export type GenieMarkState = "idle" | "thinking" | "still";

const BRACE_L = "M70 50 C55 50 58 62 58 78 C58 92 52 98 44 100 C52 102 58 108 58 122 C58 138 55 150 70 150";
const BRACE_R = "M130 50 C145 50 142 62 142 78 C142 92 148 98 156 100 C148 102 142 108 142 122 C142 138 145 150 130 150";
const STAR = "M100 74 C103 92 108 97 126 100 C108 103 103 108 100 126 C97 108 92 103 74 100 C92 97 97 92 100 74 Z";
const STAR_BOLD = "M100 68 C104 90 110 96 132 100 C110 104 104 110 100 132 C96 110 90 104 68 100 C90 96 96 90 100 68 Z";

export function GenieMark({ size = 20, state = "idle", label }: { size?: number; state?: GenieMarkState; label?: string }) {
  const bold = size <= 24;
  return (
    <svg
      className={`genie-mark genie-mark--${state}`}
      width={size}
      height={size}
      viewBox="30 30 140 140"
      role={label ? "img" : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
    >
      <g fill="none" stroke="currentColor" strokeWidth={bold ? 14 : 8} strokeLinecap="round" strokeLinejoin="round">
        <path className="genie-mark__l" d={BRACE_L} />
        <path className="genie-mark__r" d={BRACE_R} />
      </g>
      <path className="genie-mark__star" d={bold ? STAR_BOLD : STAR} />
    </svg>
  );
}
