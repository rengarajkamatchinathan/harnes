// Genie brand mark ("Split Gear"): two crossed triangles with flat-cut tips reading as a six-tooth
// gear. The upper triangle (--genie-accent, crimson) sits on the lower plate (currentColor), with a
// transparent gap cut between them. `idle` breathes the upper triangle, `thinking` turns the gear
// 120° per beat, `still` holds. Small sizes widen the gap so the split stays visible.
// Source SVGs and the app-icon masters live in apps/desktop/brand/.

import { useId } from "react";

export type GenieMarkState = "idle" | "thinking" | "still";

const UPPER = "28,9.93 36,9.93 53.11,39.57 49.11,46.5 14.89,46.5 10.89,39.57";
const LOWER = "36,54.07 28,54.07 10.89,24.43 14.89,17.5 49.11,17.5 53.11,24.43";

export function GenieMark({ size = 20, state = "idle", label }: { size?: number; state?: GenieMarkState; label?: string }) {
  const cut = `genie-cut-${useId().replace(/:/g, "")}`;
  const gap = size <= 24 ? 4.5 : 3;
  return (
    <svg
      className={`genie-mark genie-mark--${state}`}
      width={size}
      height={size}
      viewBox="2 2 60 60"
      role={label ? "img" : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
    >
      <mask id={cut} maskUnits="userSpaceOnUse" x="0" y="0" width="64" height="64">
        <rect width="64" height="64" fill="#fff" />
        <polygon points={UPPER} fill="#000" stroke="#000" strokeWidth={gap} strokeLinejoin="round" />
      </mask>
      <g className="genie-mark__gear">
        <polygon className="genie-mark__plate" points={LOWER} mask={`url(#${cut})`} />
        <polygon className="genie-mark__tri" points={UPPER} />
      </g>
    </svg>
  );
}
