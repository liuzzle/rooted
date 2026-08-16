import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Anchor,
  Book,
  ChapterAnnotations,
  Verse,
  anchorKey,
  getChapter,
  getChapterAnnotations,
  getLastRead,
  setLastRead,
  verseAnchor,
  wordAnchor,
} from "../../lib/api";
import { formatPosition, parsePosition } from "../../lib/reference";
import NotesPanel from "../notes/NotesPanel";
import ChapterNotes from "../notes/ChapterNotes";
import { ReadingTarget } from "../../App";

const EMPTY_ANNOTATIONS: ChapterAnnotations = { highlights: [], note_marks: [] };

export default function Reader({
  translationId,
  books,
  target,
  onError,
}: {
  translationId: number;
  books: Book[];
  target: ReadingTarget | null;
  onError: (message: string) => void;
}) {
  const [bookOsis, setBookOsis] = useState<string | null>(null);
  const [chapter, setChapter] = useState(1);
  const [verses, setVerses] = useState<Verse[]>([]);
  const [annotations, setAnnotations] =
    useState<ChapterAnnotations>(EMPTY_ANNOTATIONS);
  const [selection, setSelection] = useState<Anchor | null>(null);
  const [showChapterNotes, setShowChapterNotes] = useState(false);
  const [notesRevision, setNotesRevision] = useState(0);

  // Open where reading left off, falling back to the first book.
  useEffect(() => {
    let cancelled = false;
    getLastRead()
      .then((position) => {
        if (cancelled) return;
        const parsed = parsePosition(position);
        setBookOsis((current) => current ?? parsed?.bookOsis ?? null);
        if (parsed) setChapter(parsed.chapter);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  // Fall back to the first available book once books load.
  useEffect(() => {
    if (bookOsis == null && books.length > 0) setBookOsis(books[0].osis);
    else if (bookOsis != null && books.length > 0 && !books.some((b) => b.osis === bookOsis)) {
      setBookOsis(books[0].osis);
      setChapter(1);
    }
  }, [books, bookOsis]);

  // Follow jumps sent from the library or dashboard.
  useEffect(() => {
    if (!target) return;
    setBookOsis(target.bookOsis);
    setChapter(target.chapter);
    setSelection(target.selection);
  }, [target]);

  useEffect(() => {
    if (bookOsis == null) return;
    getChapter(translationId, bookOsis, chapter)
      .then(setVerses)
      .catch((e) => onError(String(e)));
    setLastRead(formatPosition(bookOsis, chapter)).catch(() => {});
  }, [translationId, bookOsis, chapter, onError]);

  // Bring the verse a jump landed on into view once the chapter has rendered.
  useEffect(() => {
    const verseId = target?.selection?.verse_id;
    if (!verseId || verses.length === 0) return;
    document
      .getElementById(`verse-${verseId}`)
      ?.scrollIntoView({ block: "center" });
  }, [target, verses]);

  const reloadAnnotations = useCallback(() => {
    if (bookOsis == null) return;
    getChapterAnnotations(translationId, bookOsis, chapter)
      .then(setAnnotations)
      .catch((e) => onError(String(e)));
  }, [translationId, bookOsis, chapter, onError]);

  useEffect(reloadAnnotations, [reloadAnnotations, notesRevision]);

  /** A note or highlight changed: refresh the pane and any open note list. */
  function annotationsChanged() {
    setNotesRevision((n) => n + 1);
  }

  // Fast lookups keyed the same way anchors are, so a verse annotation matches
  // by verse id alone and a word annotation by (verse id, token index).
  const highlightByAnchor = useMemo(() => {
    const m = new Map<string, string>();
    for (const h of annotations.highlights) {
      m.set(
        h.anchor_type === "word"
          ? `w:${h.verse_id}:${h.token_idx}`
          : `v:${h.verse_id}`,
        h.color,
      );
    }
    return m;
  }, [annotations]);

  const notesByAnchor = useMemo(() => {
    const m = new Map<string, { count: number; degraded: number }>();
    for (const n of annotations.note_marks) {
      m.set(
        n.token_idx === null
          ? `v:${n.verse_id}`
          : `w:${n.verse_id}:${n.token_idx}`,
        { count: n.count, degraded: n.degraded },
      );
    }
    return m;
  }, [annotations]);

  const bookIndex = books.findIndex((b) => b.osis === bookOsis);
  const activeBook = bookIndex >= 0 ? books[bookIndex] : null;

  /** Previous/next chapter, crossing book boundaries. */
  const prev = useMemo(() => {
    if (!activeBook) return null;
    if (chapter > 1) return { book: activeBook, chapter: chapter - 1 };
    const before = books[bookIndex - 1];
    return before ? { book: before, chapter: before.chapter_count } : null;
  }, [activeBook, books, bookIndex, chapter]);

  const next = useMemo(() => {
    if (!activeBook) return null;
    if (chapter < activeBook.chapter_count)
      return { book: activeBook, chapter: chapter + 1 };
    const after = books[bookIndex + 1];
    return after ? { book: after, chapter: 1 } : null;
  }, [activeBook, books, bookIndex, chapter]);

  function goTo(step: { book: Book; chapter: number } | null) {
    if (!step) return;
    setBookOsis(step.book.osis);
    setChapter(step.chapter);
    setSelection(null);
    document.querySelector(".verses")?.scrollTo({ top: 0 });
  }

  const ot = books.filter((b) => b.testament === "OT");
  const nt = books.filter((b) => b.testament === "NT");

  function selectBook(osis: string) {
    setBookOsis(osis);
    setChapter(1);
    setSelection(null);
  }

  /** Clicking the same anchor twice closes the panel. */
  function toggleSelection(anchor: Anchor) {
    setSelection((cur) =>
      cur && anchorKey(cur) === anchorKey(anchor) ? null : anchor,
    );
  }

  const selectionLabel = useMemo(() => {
    if (!selection) return "";
    return selection.anchor_type === "word"
      ? `“${selection.surface}” · ${selection.verse_id}`
      : selection.verse_id;
  }, [selection]);

  return (
    <div className="reader">
      {/* Sidebar: book navigation */}
      <aside className="sidebar">
        <div className="book-group">
          <h3>Old Testament</h3>
          {ot.map((b) => (
            <button
              key={b.osis}
              className={b.osis === bookOsis ? "book active" : "book"}
              onClick={() => selectBook(b.osis)}
            >
              {b.name}
            </button>
          ))}
        </div>
        <div className="book-group">
          <h3>New Testament</h3>
          {nt.map((b) => (
            <button
              key={b.osis}
              className={b.osis === bookOsis ? "book active" : "book"}
              onClick={() => selectBook(b.osis)}
            >
              {b.name}
            </button>
          ))}
        </div>
      </aside>

      {/* Main reading pane */}
      <main className="reading">
        <header className="reading-header">
          <div className="ref">
            <h2>
              {activeBook?.name} {chapter}
            </h2>
          </div>
          <div className="controls">
            <button
              className="ghost-btn"
              onClick={() => goTo(prev)}
              disabled={!prev}
              title={prev ? `${prev.book.name} ${prev.chapter}` : "Start of the Bible"}
            >
              ‹
            </button>
            <select
              value={chapter}
              onChange={(e) => setChapter(Number(e.target.value))}
            >
              {Array.from({ length: activeBook?.chapter_count ?? 1 }, (_, i) => i + 1).map(
                (n) => (
                  <option key={n} value={n}>
                    Ch. {n}
                  </option>
                ),
              )}
            </select>
            <button
              className="ghost-btn"
              onClick={() => goTo(next)}
              disabled={!next}
              title={next ? `${next.book.name} ${next.chapter}` : "End of the Bible"}
            >
              ›
            </button>
            <button
              className={showChapterNotes ? "ghost-btn active" : "ghost-btn"}
              onClick={() => setShowChapterNotes((v) => !v)}
            >
              Chapter notes
            </button>
          </div>
        </header>

        <div className="verses">
          {verses.map((v) => {
            const vKey = `v:${v.verse_id}`;
            const verseHl = highlightByAnchor.get(vKey);
            const verseNotes = notesByAnchor.get(vKey);
            const isSelected =
              selection?.anchor_type === "verse" &&
              selection.verse_id === v.verse_id;
            return (
              <p
                key={v.verse_id}
                id={`verse-${v.verse_id}`}
                className={[
                  "verse",
                  isSelected ? "selected" : "",
                  verseHl ? `hl-${verseHl}` : "",
                ]
                  .filter(Boolean)
                  .join(" ")}
              >
                <sup
                  className="vnum"
                  onClick={() => toggleSelection(verseAnchor(v.verse_id))}
                  title={v.verse_id}
                >
                  {v.verse}
                </sup>
                {verseNotes && (
                  <span
                    className={
                      verseNotes.degraded > 0 ? "note-dot degraded" : "note-dot"
                    }
                    title={noteDotTitle(verseNotes)}
                    onClick={() => toggleSelection(verseAnchor(v.verse_id))}
                  />
                )}
                <VerseText
                  verse={v}
                  translationId={translationId}
                  selection={selection}
                  highlightByAnchor={highlightByAnchor}
                  notesByAnchor={notesByAnchor}
                  onWord={toggleSelection}
                />
              </p>
            );
          })}

          {verses.length > 0 && (
            <nav className="chapter-nav">
              <button
                className="ghost-btn"
                onClick={() => goTo(prev)}
                disabled={!prev}
              >
                ‹ {prev ? `${prev.book.name} ${prev.chapter}` : "Start"}
              </button>
              <button
                className="ghost-btn"
                onClick={() => goTo(next)}
                disabled={!next}
              >
                {next ? `${next.book.name} ${next.chapter}` : "End"} ›
              </button>
            </nav>
          )}
        </div>
      </main>

      {selection ? (
        <NotesPanel
          anchor={selection}
          translationId={translationId}
          label={selectionLabel}
          highlight={highlightByAnchor.get(anchorKey(selection)) ?? null}
          verseNoteCount={verseNoteCount(notesByAnchor, selection.verse_id)}
          onShowVerse={() => setSelection(verseAnchor(selection.verse_id))}
          onChanged={annotationsChanged}
          onClose={() => setSelection(null)}
        />
      ) : (
        showChapterNotes &&
        bookOsis != null && (
          <ChapterNotes
            translationId={translationId}
            bookOsis={bookOsis}
            bookName={activeBook?.name ?? bookOsis}
            chapter={chapter}
            revision={notesRevision}
            onSelect={setSelection}
            onClose={() => setShowChapterNotes(false)}
          />
        )
      )}
    </div>
  );
}

/** How many notes sit on the verse itself (including degraded word notes). */
function verseNoteCount(
  marks: Map<string, { count: number; degraded: number }>,
  verseId: string,
): number {
  const mark = marks.get(`v:${verseId}`);
  return mark ? mark.count + mark.degraded : 0;
}

function noteDotTitle({
  count,
  degraded,
}: {
  count: number;
  degraded: number;
}): string {
  const parts: string[] = [];
  if (count > 0) parts.push(`${count} verse note${count > 1 ? "s" : ""}`);
  if (degraded > 0)
    parts.push(
      `${degraded} word note${degraded > 1 ? "s" : ""} from another translation`,
    );
  return parts.join(" · ");
}

/**
 * Render a verse as text with each word individually clickable. We interleave
 * the token surfaces with the raw text between them (spaces, punctuation), so
 * the displayed text is exactly the source text — nothing invented or dropped.
 */
function VerseText({
  verse,
  translationId,
  selection,
  highlightByAnchor,
  notesByAnchor,
  onWord,
}: {
  verse: Verse;
  translationId: number;
  selection: Anchor | null;
  highlightByAnchor: Map<string, string>;
  notesByAnchor: Map<string, { count: number; degraded: number }>;
  onWord: (anchor: Anchor) => void;
}) {
  const { text, tokens } = verse;
  const parts: React.ReactNode[] = [];
  let cursor = 0;

  tokens.forEach((tok) => {
    if (tok.char_start > cursor) {
      parts.push(<span key={`gap-${cursor}`}>{text.slice(cursor, tok.char_start)}</span>);
    }
    const key = `w:${verse.verse_id}:${tok.idx}`;
    const isSel =
      selection?.anchor_type === "word" &&
      selection.verse_id === verse.verse_id &&
      selection.token_idx === tok.idx;
    const hl = highlightByAnchor.get(key);
    const hasNote = (notesByAnchor.get(key)?.count ?? 0) > 0;
    parts.push(
      <span
        key={`tok-${tok.idx}`}
        className={[
          "word",
          isSel ? "selected" : "",
          hl ? `hl-${hl}` : "",
          hasNote ? "has-note" : "",
        ]
          .filter(Boolean)
          .join(" ")}
        onClick={() =>
          onWord(wordAnchor(verse.verse_id, translationId, tok.idx, tok.surface))
        }
      >
        {text.slice(tok.char_start, tok.char_end)}
      </span>,
    );
    cursor = tok.char_end;
  });
  if (cursor < text.length) {
    parts.push(<span key={`gap-end`}>{text.slice(cursor)}</span>);
  }

  return <span className="verse-text">{parts} </span>;
}
