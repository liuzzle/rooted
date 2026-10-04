import { useCallback, useEffect, useState } from "react";
import {
  MergeSide,
  MergeSuggestion,
  markConceptsDistinct,
  mergeConcepts,
  mergeSuggestions,
} from "../../lib/api";
import Marked from "../notes/Marked";

/**
 * Topics that may be one — offered, never merged on their own.
 *
 * The suggestion comes from how close the two labels are in meaning, which
 * catches variants and near-synonyms ("Opfer" / "Opfergabe") but not every
 * paraphrase. Each side shows a passage that says it, because the decision is
 * about what the notes mean, and the notes are the evidence.
 */
export default function MergeReview({ onChanged }: { onChanged: () => void }) {
  const [pairs, setPairs] = useState<MergeSuggestion[]>([]);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    mergeSuggestions()
      .then(setPairs)
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(refresh, [refresh]);

  async function act(run: () => Promise<void>) {
    setBusy(true);
    try {
      await run();
      refresh();
      onChanged();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  if (error) return <p className="notes-error">{error}</p>;
  if (pairs.length === 0) return null;

  return (
    <section className="merge-review">
      <button className="link-btn merge-review-toggle" onClick={() => setOpen((o) => !o)}>
        {open ? "▾" : "▸"} {pairs.length} pair{pairs.length === 1 ? "" : "s"} of topics may
        be the same
      </button>
      {open && (
        <ul className="merge-pairs">
          {pairs.map((p) => (
            <li key={`${p.a.concept_id}-${p.b.concept_id}`} className="merge-pair">
              <div className="merge-sides">
                <Side side={p.a} />
                <Side side={p.b} />
              </div>
              <div className="merge-actions">
                <button
                  className="ghost-btn"
                  disabled={busy}
                  onClick={() => act(() => mergeConcepts(p.b.concept_id, p.a.concept_id))}
                >
                  Keep “{p.a.label}”
                </button>
                <button
                  className="ghost-btn"
                  disabled={busy}
                  onClick={() => act(() => mergeConcepts(p.a.concept_id, p.b.concept_id))}
                >
                  Keep “{p.b.label}”
                </button>
                <button
                  className="link-btn"
                  disabled={busy}
                  onClick={() => act(() => markConceptsDistinct(p.a.concept_id, p.b.concept_id))}
                >
                  not the same
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function Side({ side }: { side: MergeSide }) {
  const shown = side.sample ? excerpt(side.sample, side.sample_marks) : null;
  return (
    <div className="merge-side">
      <h4>
        {side.label}{" "}
        <span className="topic-count">
          {side.notes} note{side.notes === 1 ? "" : "s"}
        </span>
      </h4>
      {shown && <Marked text={shown.text} marks={shown.marks} />}
    </div>
  );
}

/** Characters of context kept either side of the mention. */
const CONTEXT = 90;

/**
 * The part of a passage around its first mark, with the marks moved to match.
 * Cut by characters (code points), as the offsets are counted.
 */
function excerpt(text: string, marks: [number, number][]) {
  const chars = Array.from(text);
  const [start, end] = marks[0] ?? [0, 0];
  const from = Math.max(0, start - CONTEXT);
  const to = Math.min(chars.length, end + CONTEXT);
  const lead = from > 0 ? "…" : "";
  return {
    text: lead + chars.slice(from, to).join("") + (to < chars.length ? "…" : ""),
    marks: marks.map(([s, e]) => [s - from + lead.length, e - from + lead.length]) as [
      number,
      number,
    ][],
  };
}
