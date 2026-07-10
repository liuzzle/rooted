import { useEffect, useMemo, useState } from "react";
import {
  Book,
  Translation,
  Verse,
  getChapter,
  listBooks,
  listTranslations,
} from "../../lib/api";

interface WordSelection {
  verseId: string;
  tokenIdx: number;
  surface: string;
}

export default function Reader() {
  const [translations, setTranslations] = useState<Translation[]>([]);
  const [translationId, setTranslationId] = useState<number | null>(null);
  const [books, setBooks] = useState<Book[]>([]);
  const [bookOsis, setBookOsis] = useState<string | null>(null);
  const [chapter, setChapter] = useState(1);
  const [verses, setVerses] = useState<Verse[]>([]);
  const [selected, setSelected] = useState<WordSelection | null>(null);
  const [selectedVerse, setSelectedVerse] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Load translations once.
  useEffect(() => {
    listTranslations()
      .then((ts) => {
        setTranslations(ts);
        if (ts.length > 0) setTranslationId(ts[0].id);
      })
      .catch((e) => setError(String(e)));
  }, []);

  // Load books when the active translation changes.
  useEffect(() => {
    if (translationId == null) return;
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

  const activeBook = useMemo(
    () => books.find((b) => b.osis === bookOsis) ?? null,
    [books, bookOsis],
  );

  const ot = books.filter((b) => b.testament === "OT");
  const nt = books.filter((b) => b.testament === "NT");

  function selectBook(osis: string) {
    setBookOsis(osis);
    setChapter(1);
    setSelected(null);
    setSelectedVerse(null);
  }

  if (error) {
    return <div className="empty">Error: {error}</div>;
  }

  if (translations.length === 0) {
    return (
      <div className="empty">
        <h2>No Bible installed yet</h2>
        <p>Import a translation, then reopen the app:</p>
        <pre>python3 scripts/import_bible.py</pre>
      </div>
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
              onChange={(e) => setTranslationId(Number(e.target.value))}
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
          </div>
        </header>

        <div className="verses">
          {verses.map((v) => (
            <p
              key={v.verse_id}
              className={selectedVerse === v.verse_id ? "verse selected" : "verse"}
            >
              <sup
                className="vnum"
                onClick={() =>
                  setSelectedVerse(selectedVerse === v.verse_id ? null : v.verse_id)
                }
                title={v.verse_id}
              >
                {v.verse}
              </sup>
              <VerseText
                verse={v}
                selected={selected}
                onWord={(tokenIdx, surface) =>
                  setSelected(
                    selected?.verseId === v.verse_id && selected?.tokenIdx === tokenIdx
                      ? null
                      : { verseId: v.verse_id, tokenIdx, surface },
                  )
                }
              />
            </p>
          ))}
        </div>

        {(selected || selectedVerse) && (
          <footer className="statusbar">
            {selected
              ? `Word selected: "${selected.surface}" — ${selected.verseId} · token ${selected.tokenIdx}`
              : `Verse selected: ${selectedVerse}`}
            <span className="hint"> (notes & highlights arrive in Phase 1)</span>
          </footer>
        )}
      </main>
    </div>
  );
}

/**
 * Render a verse as text with each word individually clickable. We interleave
 * the token surfaces with the raw text between them (spaces, punctuation), so
 * the displayed text is exactly the source text — nothing invented or dropped.
 */
function VerseText({
  verse,
  selected,
  onWord,
}: {
  verse: Verse;
  selected: WordSelection | null;
  onWord: (tokenIdx: number, surface: string) => void;
}) {
  const { text, tokens } = verse;
  const parts: React.ReactNode[] = [];
  let cursor = 0;

  tokens.forEach((tok) => {
    if (tok.char_start > cursor) {
      parts.push(<span key={`gap-${cursor}`}>{text.slice(cursor, tok.char_start)}</span>);
    }
    const isSel =
      selected?.verseId === verse.verse_id && selected?.tokenIdx === tok.idx;
    parts.push(
      <span
        key={`tok-${tok.idx}`}
        className={isSel ? "word selected" : "word"}
        onClick={() => onWord(tok.idx, tok.surface)}
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
