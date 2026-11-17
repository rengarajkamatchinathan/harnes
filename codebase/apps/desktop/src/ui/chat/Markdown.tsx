import { memo, useEffect, useRef } from "react";
import type { Citation } from "@genie/protocol";
import { BlockSplitter, renderMermaidIn, renderSettled, renderTail, splitAll, withCitations } from "../../markdown/markdown";
import type { PartView } from "../../state/sessionModel";

const Block = memo(function Block({ src, settled }: { src: string; settled: boolean }) {
  const ref = useRef<HTMLDivElement>(null);
  const html = settled ? renderSettled(src) : renderTail(src);
  useEffect(() => {
    if (settled) renderMermaidIn(ref.current); // Mermaid only once the block has settled
  }, [html, settled]);
  // The unstable tail is hidden from screen readers; settled blocks are announced (05 §11).
  return <div ref={ref} className="md" aria-hidden={settled ? undefined : true} dangerouslySetInnerHTML={{ __html: html }} />;
});

/** Final Markdown, split into blocks so each is parsed once and cached. */
export const MarkdownText = memo(function MarkdownText({ text, citations }: { text: string; citations?: Citation[] }) {
  const blocks = splitAll(withCitations(text, citations ?? []));
  return (
    <div className="markdown">
      {blocks.map((b, i) => (
        <Block key={i} src={b} settled />
      ))}
    </div>
  );
});

// One splitter per streaming part, fed with what arrived since the last render.
const splitters = new WeakMap<PartView, { s: BlockSplitter; consumed: number }>();

/**
 * A text part, streaming or final, in one component so finishing a reply never remounts it
 * (that swap was a visible flash). Settled blocks are memoised; only the tail re-renders
 * (05 §7). Block keys are positional, so when the tail settles it updates in place.
 */
export function TextPart({ part, citations }: { part: PartView; citations?: Citation[] }) {
  let blocks: string[];
  let settledCount: number;
  if (part.streaming) {
    let st = splitters.get(part);
    if (!st) {
      st = { s: new BlockSplitter(), consumed: 0 };
      splitters.set(part, st);
    }
    if (part.text.length > st.consumed) {
      st.s.push(part.text.slice(st.consumed));
      st.consumed = part.text.length;
    }
    const tail = st.s.tail();
    blocks = tail ? [...st.s.settled, tail] : st.s.settled;
    settledCount = st.s.settled.length;
  } else {
    blocks = splitAll(withCitations(part.text, citations ?? []));
    settledCount = blocks.length;
  }
  return (
    <div className="markdown">
      {blocks.map((b, i) => (
        <Block key={i} src={b} settled={i < settledCount} />
      ))}
    </div>
  );
}
