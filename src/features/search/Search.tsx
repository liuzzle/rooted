import { useEffect, useRef, useState } from "react";
import {
  Anchor,
  FoundBy,
  Meaning,
  NoteHit,
  SearchResults,
  VerseHit,
  search,
  verseAnchor,
} from "../../lib/api";
import { formatReference } from "../../lib/reference";
import Marked from "../notes/Marked";

/**
 * Search.
 *
 * Two lanes, your notes and the Bible, and nothing else on screen but what they
 * found: each result is a passage quoted whole, with where it came from and how
 * it was found. There is no answer box, because there is nothing that could
 * write an answer — and "no sources found" is shown as a result, not an error.
 */
export default function Search({
  translationId,
  translationAbbrev,
  onOpenNote,
  onJump,
}: {
  translationId: number;
  translationAbbrev: string | null;
  onOpenNote: (noteId: number) => void;
  onJump: (bookOsis: string, chapter: number, selection: Anchor | null) => void;
}) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<SearchResults | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // Only the newest query may land: a slow model reply for "gna" must not
  // overwrite the results for "gnade".
  const latest = useRef(0);

  useEffect(() => {
    const text = query.trim();
    if (!text) {
      setResults(null);
      return;
    }
    const ticket = ++latest.current;
    const id = setTimeout(() => {
      setBusy(true);
      search(text, translationId)
        .then((found) => {
          if (ticket !== latest.current) return;
          setResults(found);
          setError(null);
        })
        .catch((e) => ticket === latest.current && setError(String(e)))
        .finally(() => ticket === latest.current && setBusy(false));
    }, 300);
    return () => clearTimeout(id);
  }, [query, translationId]);

  const nothing =
    results && results.notes.length === 0 && results.bible.length === 0;

  return (
    <div className="search-view">
      <header className="search-header">
        <input
          className="search-box"
          autoFocus
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search your notes and the Bible — a word, a topic, a phrase"
        />
        {results && <MeaningStatus meaning={results.meaning} />}
      </header>

      {error && <p className="notes-error">{error}</p>}

      {!results && !error && (
        <p className="card-note search-intro">
          Results are passages quoted from your notes and from{" "}
          {translationAbbrev ?? "the open translation"}, each with where it
          came from. Nothing is summarised or written for you.
        </p>
      )}

      {nothing && !busy && (
        <p className="empty search-nothing">
          No sources found for “{results.query}”.
        </p>
      )}

      {results && !nothing && (
        <div className="search-lanes">
          <section className="search-lane">
            <h3>
              Your notes <span className="lane-count">{results.notes.length}</span>
            </h3>
            {results.notes.length === 0 && (
              <p className="empty">No note mentions this.</p>
            )}
            <ul className="citation-list">
              {results.notes.map((hit, i) => (
                <NoteResult key={i} hit={hit} onOpen={onOpenNote} />
              ))}
            </ul>
          </section>
          <section className="search-lane">
            <h3>
              Bible · {translationAbbrev}{" "}
              <span className="lane-count">{results.bible.length}</span>
            </h3>
            {results.bible.length === 0 && (
              <p className="empty">No verse in {translationAbbrev} matches.</p>
            )}
            <ul className="citation-list">
              {results.bible.map((hit) => (
                <VerseResult key={hit.verse_id} hit={hit} onJump={onJump} />
              ))}
            </ul>
          </section>
        </div>
      )}
    </div>
  );
}

function NoteResult({
  hit,
  onOpen,
}: {
  hit: NoteHit;
  onOpen: (noteId: number) => void;
}) {
  return (
    <li className="citation">
      <Marked text={hit.snippet} marks={hit.marks} />
      <div className="citation-source">
        <button className="link-btn" onClick={() => onOpen(hit.note_id)}>
          {hit.note_title ?? `Note ${hit.note_id}`}
        </button>
        {hit.speaker && <span> · {hit.speaker}</span>}
        {hit.date && <span> · {hit.date}</span>}
        <FoundByChips by={hit.matched_by} />
      </div>
    </li>
  );
}

function VerseResult({
  hit,
  onJump,
}: {
  hit: VerseHit;
  onJump: (bookOsis: string, chapter: number, selection: Anchor | null) => void;
}) {
  return (
    <li className="citation">
      <Marked text={hit.snippet} marks={hit.marks} />
      <div className="citation-source">
        <button
          className="link-btn"
          onClick={() => onJump(hit.book_osis, hit.chapter, verseAnchor(hit.verse_id))}
        >
          {formatReference(hit.book_name, hit.book_osis, hit.chapter, hit.verse)}
        </button>
        {hit.cited_by > 0 && (
          <span>
            {" "}
            · cited in {hit.cited_by} matching note{hit.cited_by === 1 ? "" : "s"}
          </span>
        )}
        <FoundByChips by={hit.matched_by} />
      </div>
    </li>
  );
}

const FOUND_BY: Record<FoundBy, [string, string]> = {
  words: ["words", "the words you typed appear here"],
  topic: ["topic", "a topic of this name is mentioned here"],
  cited: ["cited", "a matching note cites this verse"],
  meaning: ["meaning", "close in meaning, though the words may differ"],
};

function FoundByChips({ by }: { by: FoundBy[] }) {
  return (
    <span className="found-by">
      {by.map((b) => (
        <span key={b} className={`found-chip found-${b}`} title={FOUND_BY[b][1]}>
          {FOUND_BY[b][0]}
        </span>
      ))}
    </span>
  );
}

/** Say whether meaning took part — a thinner list should explain itself. */
function MeaningStatus({ meaning }: { meaning: Meaning }) {
  if (!meaning.used) {
    return (
      <p className="meaning-status off">
        Words and topics only{meaning.detail ? ` — ${meaning.detail}` : ""}.
      </p>
    );
  }
  const partial =
    meaning.notes_read < meaning.notes_total ||
    meaning.verses_read < meaning.verses_total;
  return (
    <p className="meaning-status">
      Also by meaning ({meaning.model})
      {partial &&
        ` — still reading: ${meaning.notes_read}/${meaning.notes_total} note passages, ` +
          `${meaning.verses_read.toLocaleString()}/${meaning.verses_total.toLocaleString()} verses`}
      .
    </p>
  );
}
