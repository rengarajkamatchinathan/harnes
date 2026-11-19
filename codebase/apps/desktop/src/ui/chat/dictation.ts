// Voice input for the composer: the webview's Web Speech API (SpeechRecognition), dictating
// into the text box. Interim words show live; final words are kept. No audio leaves the
// webview through Genie — the platform's speech service does the recognition.
import { useCallback, useEffect, useRef, useState } from "react";

// Minimal typings: the Web Speech API is not in TypeScript's DOM lib.
interface RecognitionResult {
  isFinal: boolean;
  0: { transcript: string };
}
interface RecognitionEvent {
  resultIndex: number;
  results: ArrayLike<RecognitionResult>;
}
interface Recognition {
  continuous: boolean;
  interimResults: boolean;
  lang: string;
  onresult: ((e: RecognitionEvent) => void) | null;
  onerror: ((e: { error: string }) => void) | null;
  onend: (() => void) | null;
  start(): void;
  stop(): void;
  abort(): void;
}
type RecognitionCtor = new () => Recognition;

function recognitionCtor(): RecognitionCtor | null {
  if (typeof window === "undefined") return null;
  const w = window as unknown as { SpeechRecognition?: RecognitionCtor; webkitSpeechRecognition?: RecognitionCtor };
  return w.SpeechRecognition ?? w.webkitSpeechRecognition ?? null;
}

export type DictationError = "unsupported" | "denied" | "failed";

/**
 * `start(base)` begins dictating after `base` (the text already in the box); every update
 * calls `onText(base + spoken)`. `stop()` ends it; it also ends on its own after silence.
 */
export function useDictation(onText: (text: string) => void, onError: (kind: DictationError, detail?: string) => void) {
  const [listening, setListening] = useState(false);
  const rec = useRef<Recognition | null>(null);
  const cb = useRef({ onText, onError });
  cb.current = { onText, onError };
  const supported = recognitionCtor() !== null;

  const stop = useCallback(() => rec.current?.stop(), []);

  const start = useCallback((base: string) => {
    const Ctor = recognitionCtor();
    if (!Ctor) return cb.current.onError("unsupported");
    rec.current?.abort();
    const r = new Ctor();
    r.continuous = true;
    r.interimResults = true;
    r.lang = navigator.language || "en-US";
    const prefix = base && !/\s$/.test(base) ? `${base} ` : base;
    let finalText = "";
    r.onresult = (e) => {
      let interim = "";
      for (let i = e.resultIndex; i < e.results.length; i++) {
        const res = e.results[i];
        if (res.isFinal) finalText += res[0].transcript;
        else interim += res[0].transcript;
      }
      cb.current.onText(prefix + (finalText + interim).replace(/^\s+/, ""));
    };
    r.onerror = (e) => {
      if (e.error === "aborted" || e.error === "no-speech") return;
      cb.current.onError(e.error === "not-allowed" || e.error === "service-not-allowed" ? "denied" : "failed", e.error);
    };
    r.onend = () => {
      if (rec.current === r) rec.current = null;
      setListening(false);
    };
    rec.current = r;
    try {
      r.start();
      setListening(true);
    } catch (err) {
      rec.current = null;
      cb.current.onError("failed", String((err as Error)?.message ?? err));
    }
  }, []);

  // Never leave the microphone open when the composer goes away.
  useEffect(() => () => rec.current?.abort(), []);

  return { listening, supported, start, stop };
}
