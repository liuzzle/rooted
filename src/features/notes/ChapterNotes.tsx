import { useEffect, useState } from "react";
import {
  Anchor,
  LibraryNote,
  listChapterNotes,
  verseAnchor,
  wordAnchor,
} from "../../lib/api";

/**
 * Every note in the chapter being read, in verse order — including word notes
 * written in another translation, which belong to the verse here.
 */
export default function ChapterNotes({
  translationId,
  bookOsis,
  bookName,
  chapter,
  revision,
  onSelect,
  onClose,
}: {
  translationId: number;
  bookOsis: string;
  bookName: string;
  chapter: number;
  /** Bumped by the reader whenever a note changes, to force a reload. */
  revision: number;
  onSelect: (anchor: Anchor) => void;
  onClose: () => void;
}) {
  const [notes, setNotes] = useState<LibraryNote[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    listChapterNotes(translationId, bookOsis, chapter)
      .then(setNotes)
      .catch((e) => setError(String(e)));
  }, [translationId, bookOsis, chapter, revision]);

  /** Open the note where it actually lives — its word, or its verse. */
  function select(note: LibraryNote) {
    const a = note.anchor;
    if (!a) return;
    onSelect(
      a.anchor_type === "word" && !a.degraded && a.translation_id != null
        ? wordAnchor(a.verse_id, a.translation_id, a.token_idx ?? 0, a.surface ?? "")
        : verseAnchor(a.verse_id),
    );
  }

  return (
    <aside className="notes-panel">
      <header className="notes-header">
        <div>
          <span className="notes-kind">Chapter</span>
          <h3>
            {bookName} {chapter}
          </h3>
        </div>
        <button className="icon-btn" onClick={onClose} title="Close">
          ×
        </button>
      </header>

      {error && <p className="notes-error">{error}</p>}

      <div className="notes-list">
        {notes.length === 0 && !error && (
          <p className="notes-empty">No notes in this chapter yet.</p>
        )}
        {notes.map((n) => (
          <button
            key={n.note_id}
            className={n.anchor?.degraded ? "note chapter degraded" : "note chapter"}
            onClick={() => select(n)}
          >
            <div className="chapter-note-ref">
              v{n.anchor?.verse}
              {n.anchor?.anchor_type === "word" && (
                <span className="chapter-note-word"> “{n.anchor.surface}”</span>
              )}
              {n.anchor?.degraded && (
                <span className="chapter-note-origin">
                  {" "}
                  · {n.anchor.origin_abbrev ?? "another translation"}
                </span>
              )}
            </div>
            {n.title && <h4>{n.title}</h4>}
            <p className="note-body">{n.body}</p>
          </button>
        ))}
      </div>
    </aside>
  );
}
