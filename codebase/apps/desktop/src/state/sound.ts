// The completion sound (Settings → General): a soft two-note chime made with Web Audio, so
// no audio file ships with the app. Silent where Web Audio is missing (tests).

let ctx: AudioContext | null = null;

export function playChime(): void {
  try {
    const Ctx = (globalThis as { AudioContext?: typeof AudioContext }).AudioContext;
    if (!Ctx) return;
    ctx ??= new Ctx();
    void ctx.resume();
    const now = ctx.currentTime;
    for (const [i, freq] of [880, 1318.5].entries()) {
      const start = now + i * 0.11;
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = "sine";
      osc.frequency.value = freq;
      gain.gain.setValueAtTime(0, start);
      gain.gain.linearRampToValueAtTime(0.12, start + 0.015);
      gain.gain.exponentialRampToValueAtTime(0.0001, start + 0.45);
      osc.connect(gain).connect(ctx.destination);
      osc.start(start);
      osc.stop(start + 0.5);
    }
  } catch {
    // Audio is a nicety; never let it break event handling.
  }
}
