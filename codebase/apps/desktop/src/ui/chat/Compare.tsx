// Model race UI: the "Compare" picker by the model picker, the composer chip, the side-by-side
// comparison of one prompt's answers ("Keep this" switches the branch), and the reopen button.
import { Button, Chip, Icon, IconButton, Menu } from "@genie/ui";
import { t } from "../../i18n";
import { useEmitter } from "../../state/emitter";
import { answerLeaf, cancelRace, keepAnswer, MAX_RACE_MODELS, promptOf, raceBook, raceModels, raceProgress, toggleExtra, turnMessages, type Race, type RaceAnswer } from "../../state/race";
import type { SessionStore } from "../../state/session";
import type { MessageView } from "../../state/sessionModel";
import { useApp } from "../context";
import { TextPart } from "./Markdown";
import { UsageChip } from "./Usage";

/** The models the next send will race (the current one first); fewer than two = no race. */
export function useRaceModels(current: string): string[] {
  const app = useApp();
  useEmitter(raceBook);
  return raceModels(current, raceBook.extras, app.usableModels.map((m) => m.id));
}

/** "Compare" next to the model picker: pick up to two more models to answer the next message. */
export function CompareToggle({ store }: { store: SessionStore | null }) {
  const app = useApp();
  useEmitter(raceBook);
  const current = store?.model.info?.model ?? app.preferredModel() ?? "";
  const others = app.usableModels.filter((m) => m.id !== current);
  const extras = raceBook.extras.filter((id) => others.some((m) => m.id === id));
  const multi = new Set(others.map((m) => m.provider)).size > 1;
  if (!current) return null;
  return (
    <Menu
      label={t("race.compare")}
      placement="up"
      align="right"
      className="modelpick__list"
      header={t("race.compareWith", { n: MAX_RACE_MODELS - 1 })}
      items={
        others.length === 0
          ? [{ id: "none", label: t("race.noModels"), disabled: true, onSelect: () => {} }]
          : [
              ...others.map((m) => ({
                id: m.id,
                label: m.name || m.id,
                group: multi ? m.provider : undefined,
                checked: extras.includes(m.id),
                disabled: !extras.includes(m.id) && extras.length >= MAX_RACE_MODELS - 1,
                onSelect: () => raceBook.setExtras(toggleExtra(extras, m.id, current)),
              })),
              ...(extras.length ? [{ id: "off", label: t("race.stopComparing"), icon: "x" as const, separatorBefore: true, onSelect: () => raceBook.setExtras([]) }] : []),
            ]
      }
      trigger={(p) => (
        <button className={`textbtn textbtn--muted comparebtn ${extras.length ? "is-on" : ""}`} aria-label={t("race.compare")} title={t("race.compare")} {...p}>
          <Icon name="compare" size={14} />
          {extras.length > 0 && <span className="comparebtn__n">{extras.length + 1}</span>}
        </button>
      )}
    />
  );
}

/** Composer chip: the next send is a race. */
export function CompareChip({ models }: { models: string[] }) {
  if (models.length < 2) return null;
  return (
    <Chip icon="compare" title={models.join(" · ")} removeLabel={t("race.chipRemove")} onRemove={() => raceBook.setExtras([])}>
      {t("race.chip", { n: models.length })}
    </Chip>
  );
}

/** Under a reply that was raced: reopen the comparison. */
export function CompareAnswersButton({ store, msg }: { store: SessionStore; msg: MessageView }) {
  const race = raceBook.forPrompt(store.id, promptOf(store.model, msg));
  if (!race || race.running) return null;
  return (
    <button type="button" className="textbtn textbtn--muted msg__compare" onClick={() => raceBook.show(race.id)}>
      <Icon name="compare" size={12} /> {t("race.reopen", { n: race.answers.length })}
    </button>
  );
}

/** The answers to one prompt side by side, live while the race runs. */
export function CompareView({ store, race }: { store: SessionStore; race: Race }) {
  const app = useApp();
  useEmitter(raceBook);
  const p = raceProgress(race);
  const name = (id: string) => app.models.find((m) => m.id === id)?.name || id;
  const keep = async (i: number) => {
    try {
      const ok = await keepAnswer(store, race, i);
      if (!ok) {
        const kept = race.kept != null ? race.answers[race.kept] : race.answers[race.answers.length - 1];
        app.toast({ title: t("race.keepUnsupported", { model: name(kept?.model ?? "") }), tone: "info" });
      }
      raceBook.show(null);
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
    }
  };
  return (
    <section className="compare" aria-label={t("race.title", { n: race.answers.length })}>
      <header className="compare__head">
        <Icon name="compare" />
        <h2 className="compare__title">{t("race.title", { n: race.answers.length })}</h2>
        <span className="compare__status" role="status">
          {race.running ? (
            <>
              <span className="g-spinner" aria-hidden /> {t("race.progress", { i: p.running ?? Math.min(p.settled + 1, p.total), n: p.total })}
            </>
          ) : race.cancelled ? (
            t("race.stopped")
          ) : (
            t("race.done")
          )}
        </span>
        <span className="grow" />
        {race.running && (
          <Button size="sm" icon="stop" onClick={() => cancelRace(store, race)}>
            {t("race.stop")}
          </Button>
        )}
        <IconButton icon="x" size="sm" label={t("race.close")} onClick={() => raceBook.show(null)} />
      </header>
      <div className="compare__grid" style={{ ["--compare-cols" as string]: race.answers.length }}>
        {race.answers.map((a, i) => (
          <AnswerCard key={i} answer={a} name={name(a.model)} store={store} kept={race.kept === i} canKeep={!race.running && a.status === "done" && !!answerLeaf(a)} onKeep={() => void keep(i)} />
        ))}
      </div>
    </section>
  );
}

function AnswerCard({ answer: a, name, store, kept, canKeep, onKeep }: { answer: RaceAnswer; name: string; store: SessionStore; kept: boolean; canKeep: boolean; onKeep: () => void }) {
  // While it runs, the answer streams in the live session model; once settled it is a snapshot.
  const messages = a.status === "running" && a.turnId ? turnMessages(store.model, a.turnId) : a.messages;
  const parts = messages.flatMap((m) => m.parts).filter((p) => p.kind === "text" && (p.text || p.streaming));
  const note =
    a.status === "waiting"
      ? t("race.waiting")
      : a.status === "running" && !parts.length
        ? t("race.answering")
        : a.status === "failed"
          ? (a.error ?? t("race.failed"))
          : a.status === "cancelled"
            ? t("race.cancelled")
            : a.status === "skipped"
              ? t("race.skipped")
              : a.status === "done" && !parts.length
                ? t("race.empty")
                : null;
  return (
    <article className={`compare__card compare__card--${a.status} ${kept ? "is-kept" : ""}`} aria-label={name}>
      <div className="compare__card-head">
        <Icon name="sparkle" size={14} />
        <span className="compare__model">{name}</span>
        {a.status === "running" && <span className="g-spinner" aria-hidden />}
      </div>
      <div className="compare__body">
        {parts.map((p) => (
          <TextPart key={p.id} part={p} citations={p.final?.type === "text" ? p.final.citations : undefined} />
        ))}
        {note && <p className={`compare__note ${a.status === "failed" ? "compare__note--error" : ""}`}>{note}</p>}
      </div>
      <div className="compare__foot">
        {a.usage && <UsageChip u={a.usage} />}
        <span className="grow" />
        {kept ? (
          <span className="compare__kept">
            <Icon name="check" size={14} /> {t("race.kept")}
          </span>
        ) : (
          <Button size="sm" variant="primary" disabled={!canKeep} onClick={onKeep}>
            {t("race.keep")}
          </Button>
        )}
      </div>
    </article>
  );
}
