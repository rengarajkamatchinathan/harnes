// Answer a reply's questions one step at a time: progress, option rows, a free-text box,
// Back / Next, then one reply with every answer.
import { useEffect, useRef, useState } from "react";
import { Icon, IconButton } from "@genie/ui";
import { t } from "../../i18n";
import type { SessionStore } from "../../state/session";
import { useAppStatic } from "../context";
import { formatAnswers, type Question } from "./questions";

/** Steppers the user closed ("answer in chat instead"), by message id. */
const dismissed = new Set<string>();

export function QuestionStepper({ messageId, questions, store }: { messageId: string; questions: Question[]; store: SessionStore }) {
  const app = useAppStatic();
  const [open, setOpen] = useState(!dismissed.has(messageId));
  const [step, setStep] = useState(0);
  const [picked, setPicked] = useState<string[][]>(() => questions.map(() => []));
  const [typed, setTyped] = useState<string[]>(() => questions.map(() => ""));
  const [sending, setSending] = useState(false);
  const [dir, setDir] = useState<"next" | "back">("next");
  const textRef = useRef<HTMLTextAreaElement>(null);

  const q = questions[step];
  const last = step === questions.length - 1;
  const answer = (i: number) => [...picked[i], typed[i].trim()].filter(Boolean).join(" — ");
  const hasAnswer = !!answer(step);

  // A question without options goes straight to the text box.
  useEffect(() => {
    if (open && !q.options.length) textRef.current?.focus();
  }, [step, open, q.options.length]);

  if (!open) return null;

  const toggle = (opt: string) =>
    setPicked((all) =>
      all.map((cur, i) => (i !== step ? cur : q.multi ? (cur.includes(opt) ? cur.filter((x) => x !== opt) : [...cur, opt]) : cur[0] === opt ? [] : [opt])),
    );
  const go = (to: number) => {
    setDir(to > step ? "next" : "back");
    setStep(to);
  };
  const submit = async () => {
    setSending(true);
    try {
      await store.send([{ type: "text", text: formatAnswers(questions, questions.map((_, i) => answer(i))) }]);
      dismissed.add(messageId);
      setOpen(false);
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
    } finally {
      setSending(false);
    }
  };
  const next = () => (last ? void submit() : go(step + 1));
  const close = () => {
    dismissed.add(messageId);
    setOpen(false);
  };

  return (
    <section
      className="qstep"
      aria-label={t("qstep.title")}
      onKeyDown={(e) => {
        // 1–9 pick an option (outside the text box); Enter moves on.
        const el = e.target as HTMLElement;
        if (el.tagName !== "TEXTAREA" && /^[1-9]$/.test(e.key) && q.options[Number(e.key) - 1]) {
          e.preventDefault();
          toggle(q.options[Number(e.key) - 1]);
        } else if (e.key === "Enter" && !e.shiftKey && (el.tagName !== "TEXTAREA" || !typed[step].includes("\n"))) {
          e.preventDefault();
          if (!sending) next();
        }
      }}
    >
      <header className="qstep__head">
        <span className="qstep__count">{t("qstep.count", { n: step + 1, total: questions.length })}</span>
        <IconButton icon="x" size="sm" label={t("qstep.dismiss")} onClick={close} />
      </header>
      <div className="qstep__bar" aria-hidden>
        <span style={{ width: `${((step + 1) / questions.length) * 100}%` }} />
      </div>

      <div key={step} className={`qstep__body qstep__body--${dir}`}>
        <h3 className="qstep__q">{q.text}</h3>
        {q.options.length > 0 && (
          <div className="qstep__opts" role={q.multi ? "group" : "radiogroup"} aria-label={q.text}>
            {q.options.map((o, i) => {
              const on = picked[step].includes(o);
              return (
                <button key={o} type="button" role={q.multi ? "checkbox" : "radio"} aria-checked={on} className={`qstep__opt ${on ? "is-on" : ""}`} onClick={() => toggle(o)}>
                  <span className="qstep__key" aria-hidden>
                    {i + 1}
                  </span>
                  <span className="qstep__label">{o}</span>
                  <span className={`qstep__mark ${q.multi ? "qstep__mark--box" : ""}`} aria-hidden>
                    {on && <Icon name="check" size={12} />}
                  </span>
                </button>
              );
            })}
          </div>
        )}
        <textarea
          ref={textRef}
          className="qstep__text"
          rows={2}
          value={typed[step]}
          placeholder={q.options.length ? t("qstep.other") : t("qstep.type")}
          aria-label={q.options.length ? t("qstep.other") : t("qstep.type")}
          onChange={(e) => setTyped((all) => all.map((v, i) => (i === step ? e.target.value : v)))}
        />
      </div>

      <footer className="qstep__foot">
        <button type="button" className="qstep__back" disabled={step === 0} onClick={() => go(step - 1)}>
          <Icon name="chevronLeft" size={14} /> {t("qstep.back")}
        </button>
        <span className="grow" />
        {!last && !hasAnswer && (
          <button type="button" className="qstep__skip" onClick={() => go(step + 1)}>
            {t("qstep.skip")}
          </button>
        )}
        <button type="button" className="qstep__next" disabled={sending} onClick={next}>
          {last ? t("qstep.send") : t("qstep.next")} <Icon name={last ? "send" : "chevronRight"} size={14} />
        </button>
      </footer>
    </section>
  );
}
