import { useCallback, useEffect, useState } from "react";
import {
  Anchor,
  NoteVerse,
  NoteVerses as Found,
  decideVerse,
  noteVerses,
  verseAnchor,
} from "../../lib/api";
import { formatReference } from "../../lib/reference";

/**
 * Verses linked to a note, and verses offered for it.
 *
 * An offered verse is close in meaning to one of the note's passages and is
 * never linked until a person accepts it. Close in meaning is a weak signal —
 * good verses and poor ones score alike — so each offer shows the passage it
 * was matched to, which is the honest reason it's there.
 */
export default function NoteVerses({
  noteId,
  translationId,
  onJump,
}: {
  noteId: number;
  translationId: number;
  onJump: (bookOsis: string, chapter: number, selection: Anchor | null) => void;
}) {
  const [found, setFound] = useState<Found | null>(null);
  // Suggestions are computed only once asked for: it means comparing every
  // passage of the note with every verse of the translation.
  const [asked, setAsked] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    setBusy(asked);
    noteVerses(noteId, translationId, asked)
      .then(setFound)
      .catch((e) => setError(String(e)))
      .finally(() => setBusy(false));
  }, [noteId, translationId, asked]);

  useEffect(refresh, [refresh]);

  async function decide(v: NoteVerse, status: "accepted" | "dismissed" | null) {
    try {
      await decideVerse(noteId, v.verse_id, status, v.score);
      refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  if (error) return <p className="notes-error">{error}</p>;
  if (!found) return null;

  const ref = (v: NoteVerse) => (
    <button
      className="link-btn verse-ref"
      onClick={() => onJump(v.book_osis, v.chapter, verseAnchor(v.verse_id))}
    >
      {formatReference(v.book_name, v.book_osis, v.chapter, v.verse)}
    </button>
  );

  return (
    <div className="note-verses">
      {found.accepted.length > 0 && (
        <p className="linked-verses">
          <span className="note-verses-label">Linked verses</span>
          {found.accepted.map((v) => (
            <span key={v.verse_id} className="linked-verse" title={v.text ?? undefined}>
              {ref(v)}
              <button
                className="unlink"
                title="Remove this link"
                aria-label={`Remove link to ${v.verse_id}`}
                onClick={() => decide(v, null)}
              >
                ×
              </button>
            </span>
          ))}
        </p>
      )}

      {!asked && (
        <button className="link-btn suggest-toggle" onClick={() => setAsked(true)}>
          Suggest verses close in meaning
        </button>
      )}
      {asked && busy && <p className="suggest-reason">Comparing with every verse…</p>}
      {asked && !busy && found.suggested.length === 0 && (
        <p className="suggest-reason">
          Nothing close enough to suggest — or the text hasn't been read by a
          local model yet.{" "}
          <button className="link-btn" onClick={() => setAsked(false)}>
            hide
          </button>
        </p>
      )}
      {asked && found.suggested.length > 0 && (
        <>
          <button className="link-btn suggest-toggle" onClick={() => setAsked(false)}>
            ▾ {found.suggested.length} verse
            {found.suggested.length === 1 ? "" : "s"} close in meaning — suggestions,
            often wrong; link only what belongs
          </button>
          <ul className="verse-suggestions">
            {found.suggested.map((v) => (
              <li key={v.verse_id} className="verse-suggestion">
                <div className="verse-suggestion-head">
                  {ref(v)}
                  <span className="suggest-actions">
                    <button className="ghost-btn" onClick={() => decide(v, "accepted")}>
                      Link
                    </button>
                    <button className="link-btn" onClick={() => decide(v, "dismissed")}>
                      not related
                    </button>
                  </span>
                </div>
                <blockquote className="citation-text">
                  {v.text ?? "This translation doesn't have this verse."}
                </blockquote>
                {v.passage && (
                  <p className="suggest-reason">close to: “{clip(v.passage, 120)}”</p>
                )}
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

function clip(text: string, max: number) {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}
