import { useEffect, useState } from "react";
import {
  Anchor,
  NoteReference,
  getVerseText,
  noteReferences,
  verseAnchor,
} from "../../lib/api";

/**
 * A note's text, with the scripture it cites made reachable.
 *
 * The body is never rewritten: it is split at the offsets the worker recorded
 * and the pieces are put back in order, so what you read is what you wrote,
 * character for character. A reference becomes clickable in place — hover to
 * see the verse, click to open it in the reader.
 *
 * References come from the database rather than being re-detected here. The
 * worker found them once, against the note as saved; searching the rendered
 * text again could mark something the ledger doesn't know about, and then the
 * note and the graph would disagree about what it cites.
 */
export default function NoteBody({
  noteId,
  body,
  translationId,
  onJump,
}: {
  noteId: number;
  body: string;
  translationId: number;
  onJump: (bookOsis: string, chapter: number, selection: Anchor | null) => void;
}) {
  const [refs, setRefs] = useState<NoteReference[]>([]);

  useEffect(() => {
    let live = true;
    noteReferences(noteId)
      .then((found) => live && setRefs(found))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [noteId]);

  if (refs.length === 0) return <p className="note-body">{body}</p>;

  const pieces: React.ReactNode[] = [];
  let at = 0;
  refs.forEach((ref, i) => {
    // Defensive: a stale offset (note edited, not yet re-read) must not scramble
    // the text. Skip the mark, keep the words.
    if (ref.char_start < at || ref.char_end > body.length) return;
    if (ref.char_start > at) pieces.push(body.slice(at, ref.char_start));
    pieces.push(
      <VerseReference
        key={`${ref.char_start}-${i}`}
        reference={ref}
        text={body.slice(ref.char_start, ref.char_end)}
        translationId={translationId}
        onJump={onJump}
      />,
    );
    at = ref.char_end;
  });
  pieces.push(body.slice(at));

  return <p className="note-body">{pieces}</p>;
}

function VerseReference({
  reference,
  text,
  translationId,
  onJump,
}: {
  reference: NoteReference;
  text: string;
  translationId: number;
  onJump: (bookOsis: string, chapter: number, selection: Anchor | null) => void;
}) {
  const [preview, setPreview] = useState<string | null | undefined>(undefined);

  function load() {
    if (preview !== undefined || !reference.verse_id) return;
    getVerseText(translationId, reference.verse_id)
      .then(setPreview)
      .catch(() => setPreview(null));
  }

  return (
    <span className="verse-ref-wrap" onMouseEnter={load} onFocus={load}>
      <button
        className="verse-ref"
        onClick={() =>
          onJump(
            reference.book_osis,
            reference.chapter,
            reference.verse_id ? verseAnchor(reference.verse_id) : null,
          )
        }
      >
        {/* The words as written, not a formatted version of the reference. */}
        {text}
      </button>
      {reference.verse_id && (
        <span className="verse-preview" role="tooltip">
          {preview === undefined
            ? "…"
            : preview ??
              "Not in the translation you're reading — open it to see where it lands."}
        </span>
      )}
    </span>
  );
}
