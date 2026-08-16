import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Anchor,
  Book,
  ChapterAnnotations,
  Translation,
  Verse,
  anchorKey,
  getActiveTranslation,
  getChapter,
  getChapterAnnotations,
  listBooks,
  listTranslations,
  setActiveTranslation,
  verseAnchor,
  wordAnchor,
} from "../../lib/api";
import NotesPanel from "../notes/NotesPanel";
import TranslationsPanel from "../translations/TranslationsPanel";

const EMPTY_ANNOTATIONS: ChapterAnnotations = { highlights: [], note_marks: [] };

export default function Reader() {
  const [translations, setTranslations] = useState<Translation[]>([]);
  const [translationId, setTranslationId] = useState<number | null>(null);
  const [books, setBooks] = useState<Book[]>([]);
  const [bookOsis, setBookOsis] = useState<string | null>(null);
  const [chapter, setChapter] = useState(1);
  const [verses, setVerses] = useState<Verse[]>([]);
  const [annotations, setAnnotations] =
    useState<ChapterAnnotations>(EMPTY_ANNOTATIONS);
  const [selection, setSelection] = useState<Anchor | null>(null);
  const [showPacks, setShowPacks] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /**
   * Reload installed translations and settle on one to read: the remembered
   * choice if it is still installed, otherwise whatever is left.
   */
  const refreshTranslations = useCallback(async () => {
    try {
      const [ts, remembered] = await Promise.all([
        listTranslations(),
        getActiveTranslation(),
      ]);
      setTranslations(ts);
      setTranslationId((current) => {
        if (current != null && ts.some((t) => t.id === current)) return current;
        const preferred = ts.find((t) => t.abbrev === remembered);
        return preferred?.id ?? ts[0]?.id ?? null;
      });
      setLoaded(true);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    refreshTranslations();
  }, [refreshTranslations]);

  // Load books when the active translation changes.
  useEffect(() => {
    if (translationId == null) {
      setBooks([]);
      setVerses([]);
      setAnnotations(EMPTY_ANNOTATIONS);
      return;
    }
    listBooks(translationId)
      .then((bs) => {
        setBooks(bs);
        if (bs.length > 0 && bookOsis == null) setBookOsis(bs[0].osis);
      })
      .catch((e) => setError(String(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [translationId]);

  // Load chapter when translation / book / chapter changes.
  useEffect(() => {
    if (translationId == null || bookOsis == null) return;
    getChapter(translationId, bookOsis, chapter)
      .then(setVerses)
      .catch((e) => setError(String(e)));
  }, [translationId, bookOsis, chapter]);

  const reloadAnnotations = useCallback(() => {
    if (translationId == null || bookOsis == null) return;
    getChapterAnnotations(translationId, bookOsis, chapter)
      .then(setAnnotations)
      .catch((e) => setError(String(e)));
  }, [translationId, bookOsis, chapter]);

  useEffect(reloadAnnotations, [reloadAnnotations]);

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

  const activeBook = useMemo(
    () => books.find((b) => b.osis === bookOsis) ?? null,
    [books, bookOsis],
  );

  const activeTranslation = useMemo(
    () => translations.find((t) => t.id === translationId) ?? null,
    [translations, translationId],
  );

  const ot = books.filter((b) => b.testament === "OT");
  const nt = books.filter((b) => b.testament === "NT");

  function selectBook(osis: string) {
    setBookOsis(osis);
    setChapter(1);
    setSelection(null);
  }

  /**
   * Switch translations and remember the choice.
   *
   * A word selection falls back to its verse rather than disappearing: the same
   * token index in another text is a different word, but the notes written on
   * that word are still about this verse — and that's where they now live.
   */
  function switchTranslation(id: number) {
    setTranslationId(id);
    setSelection((cur) =>
      cur?.anchor_type === "word" ? verseAnchor(cur.verse_id) : cur,
    );
    const abbrev = translations.find((t) => t.id === id)?.abbrev;
    if (abbrev) setActiveTranslation(abbrev).catch((e) => setError(String(e)));
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

  if (error) {
    return <div className="empty">Error: {error}</div>;
  }

  if (loaded && translations.length === 0) {
    return (
      <>
        <div className="empty">
          <h2>No Bible installed yet</h2>
          <p>Download a translation to start reading.</p>
          <button className="primary" onClick={() => setShowPacks(true)}>
            Browse translations
          </button>
        </div>
        {showPacks && (
          <TranslationsPanel
            activeAbbrev={null}
            onUse={() => setShowPacks(false)}
            onInstalled={refreshTranslations}
            onClose={() => setShowPacks(false)}
          />
        )}
      </>
    );
  }

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
            <select
              value={translationId ?? ""}
              onChange={(e) => switchTranslation(Number(e.target.value))}
              title={activeTranslation?.name}
            >
              {translations.map((t) => (
                <option key={t.id} value={t.id}>
                  {t.abbrev}
                </option>
              ))}
            </select>
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
            <button className="ghost-btn" onClick={() => setShowPacks(true)}>
              Translations…
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
                  translationId={translationId!}
                  selection={selection}
                  highlightByAnchor={highlightByAnchor}
                  notesByAnchor={notesByAnchor}
                  onWord={toggleSelection}
                />
              </p>
            );
          })}
        </div>
      </main>

      {selection && translationId != null && (
        <NotesPanel
          anchor={selection}
          translationId={translationId}
          label={selectionLabel}
          highlight={highlightByAnchor.get(anchorKey(selection)) ?? null}
          verseNoteCount={verseNoteCount(notesByAnchor, selection.verse_id)}
          onShowVerse={() => setSelection(verseAnchor(selection.verse_id))}
          onChanged={reloadAnnotations}
          onClose={() => setSelection(null)}
        />
      )}

      {showPacks && (
        <TranslationsPanel
          activeAbbrev={activeTranslation?.abbrev ?? null}
          onUse={(abbrev) => {
            const t = translations.find((x) => x.abbrev === abbrev);
            if (t) switchTranslation(t.id);
            setShowPacks(false);
          }}
          onInstalled={refreshTranslations}
          onClose={() => setShowPacks(false)}
        />
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
