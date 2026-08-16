import { useEffect, useMemo, useState } from "react";
import {
  Anchor,
  Book,
  LibraryNote,
  RecentHighlight,
  Stats,
  getLastRead,
  getStats,
  listAllNotes,
  listRecentHighlights,
  verseAnchor,
  wordAnchor,
} from "../../lib/api";
import {
  formatReference,
  parsePosition,
  parseReference,
} from "../../lib/reference";
import { ReferenceField } from "../library/NotesLibrary";

const CANON_BOOKS = 66;
const TREND_DAYS = 30;

export default function Dashboard({
  translationId,
  books,
  onJump,
}: {
  translationId: number;
  books: Book[];
  onJump: (bookOsis: string, chapter: number, selection: Anchor | null) => void;
}) {
  const [stats, setStats] = useState<Stats | null>(null);
  const [recentNotes, setRecentNotes] = useState<LibraryNote[]>([]);
  const [recentHighlights, setRecentHighlights] = useState<RecentHighlight[]>([]);
  const [lastRead, setLastRead] = useState<string | null>(null);
  const [jump, setJump] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    Promise.all([
      getStats(),
      listAllNotes(translationId, null, null, 5),
      listRecentHighlights(translationId, 5),
      getLastRead(),
    ])
      .then(([s, notes, highlights, position]) => {
        setStats(s);
        setRecentNotes(notes);
        setRecentHighlights(highlights);
        setLastRead(position);
      })
      .catch((e) => setError(String(e)));
  }, [translationId]);

  const resume = useMemo(() => {
    const parsed = parsePosition(lastRead);
    if (!parsed) return null;
    const book = books.find((b) => b.osis === parsed.bookOsis);
    return book ? { book, chapter: parsed.chapter } : null;
  }, [lastRead, books]);

  const parsedJump = parseReference(jump, books);

  function goToJump() {
    if (!parsedJump) return;
    onJump(
      parsedJump.book.osis,
      parsedJump.chapter,
      parsedJump.verse
        ? verseAnchor(
            `${parsedJump.book.osis}.${parsedJump.chapter}.${parsedJump.verse}`,
          )
        : null,
    );
  }

  function openNote(note: LibraryNote) {
    const a = note.anchor;
    if (!a) return;
    onJump(
      a.book_osis,
      a.chapter,
      a.anchor_type === "word" && !a.degraded && a.translation_id != null
        ? wordAnchor(a.verse_id, a.translation_id, a.token_idx ?? 0, a.surface ?? "")
        : verseAnchor(a.verse_id),
    );
  }

  return (
    <div className="dashboard">
      {error && <p className="notes-error">{error}</p>}

      <section className="dash-top">
        <div className="card resume">
          <h3>Continue reading</h3>
          {resume ? (
            <>
              <p className="resume-ref">
                {resume.book.name} {resume.chapter}
              </p>
              <button
                className="primary"
                onClick={() => onJump(resume.book.osis, resume.chapter, null)}
              >
                Open →
              </button>
            </>
          ) : (
            <p className="notes-empty">Nothing read yet.</p>
          )}

          <div className="jump">
            <ReferenceField
              value={jump}
              onChange={setJump}
              books={books}
              placeholder="Jump to — e.g. John 3:16"
            />
            <button
              className="ghost-btn"
              onClick={goToJump}
              disabled={!parsedJump}
            >
              Go
            </button>
          </div>
        </div>

        <div className="tiles">
          <Tile value={stats?.notes_total} label="notes" />
          <Tile value={stats?.highlights_total} label="highlights" />
          <Tile value={stats?.notes_standalone} label="without a reference" />
          <Tile value={stats?.translations_installed} label="translations" />
        </div>
      </section>

      <section className="dash-columns">
        <div className="card">
          <h3>Recent notes</h3>
          {recentNotes.length === 0 && <p className="notes-empty">No notes yet.</p>}
          <ul className="recent-list">
            {recentNotes.map((n) => (
              <li key={n.note_id}>
                <button
                  className="recent-item"
                  onClick={() => openNote(n)}
                  disabled={!n.anchor}
                >
                  <span className="recent-ref">
                    {n.anchor
                      ? formatReference(
                          n.anchor.book_name,
                          n.anchor.book_osis,
                          n.anchor.chapter,
                          n.anchor.verse,
                        )
                      : "No reference"}
                  </span>
                  <span className="recent-body">{n.title ?? n.body}</span>
                </button>
              </li>
            ))}
          </ul>
        </div>

        <div className="card">
          <h3>Recent highlights</h3>
          {recentHighlights.length === 0 && (
            <p className="notes-empty">No highlights yet.</p>
          )}
          <ul className="recent-list">
            {recentHighlights.map((h) => (
              <li key={h.id}>
                <button
                  className="recent-item"
                  onClick={() =>
                    onJump(
                      h.verse_id.split(".")[0],
                      h.chapter,
                      verseAnchor(h.verse_id),
                    )
                  }
                >
                  <span className="recent-ref">
                    <span className={`swatch-dot hl-${h.color}`} />
                    {formatReference(
                      h.book_name,
                      h.verse_id.split(".")[0],
                      h.chapter,
                      h.verse,
                    )}
                  </span>
                  <span className="recent-body">
                    {h.anchor_type === "word" && h.surface
                      ? `“${h.surface}”`
                      : h.text ?? ""}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </div>
      </section>

      {stats && (
        <section className="dash-analytics">
          <div className="card">
            <h3>Most annotated books</h3>
            <p className="card-note">Notes and highlights, pooled.</p>
            <BookBars counts={stats.by_book} />
          </div>

          <div className="card">
            <h3>Notes in the last {TREND_DAYS} days</h3>
            <NotesTrend counts={stats.notes_by_day} />
          </div>

          <div className="card">
            <h3>Highlights</h3>
            {stats.by_color.length === 0 ? (
              <p className="notes-empty">No highlights yet.</p>
            ) : (
              <ul className="color-list">
                {stats.by_color.map((c) => (
                  <li key={c.key}>
                    <span className={`swatch-dot hl-${c.key}`} />
                    <span className="color-name">{c.key}</span>
                    <span className="color-count">{c.count}</span>
                  </li>
                ))}
              </ul>
            )}
          </div>

          <div className="card">
            <h3>Canon covered</h3>
            <p className="hero-number">
              {stats.books_annotated}
              <span className="hero-unit"> of {CANON_BOOKS} books</span>
            </p>
            <div className="meter" role="img"
              aria-label={`${stats.books_annotated} of ${CANON_BOOKS} books annotated`}>
              <div
                className="meter-fill"
                style={{
                  width: `${(stats.books_annotated / CANON_BOOKS) * 100}%`,
                }}
              />
            </div>
          </div>
        </section>
      )}
    </div>
  );
}

function Tile({ value, label }: { value: number | undefined; label: string }) {
  return (
    <div className="tile">
      <span className="tile-value">{value ?? "—"}</span>
      <span className="tile-label">{label}</span>
    </div>
  );
}

/** Ranked magnitude: one hue, bars proportional to the largest, value labelled. */
function BookBars({ counts }: { counts: { key: string; label: string | null; count: number }[] }) {
  if (counts.length === 0) return <p className="notes-empty">Nothing annotated yet.</p>;
  const max = Math.max(...counts.map((c) => c.count));
  return (
    <ul className="bar-list">
      {counts.map((c) => (
        <li key={c.key}>
          <span className="bar-label">{c.label ?? c.key}</span>
          <span className="bar-track">
            <span
              className="bar-fill"
              style={{ width: `${(c.count / max) * 100}%` }}
              title={`${c.label ?? c.key}: ${c.count}`}
            />
          </span>
          <span className="bar-value">{c.count}</span>
        </li>
      ))}
    </ul>
  );
}

/** Local `YYYY-MM-DD` — matches the keys the query groups by. */
function localDayKey(d: Date): string {
  const month = `${d.getMonth() + 1}`.padStart(2, "0");
  const day = `${d.getDate()}`.padStart(2, "0");
  return `${d.getFullYear()}-${month}-${day}`;
}

/**
 * Notes per day over the trailing window. The query returns only days that have
 * notes, so the empty days are filled in here — otherwise the shape would lie.
 */
function NotesTrend({ counts }: { counts: { key: string; count: number }[] }) {
  const byDay = new Map(counts.map((c) => [c.key, c.count]));
  const today = new Date();
  const days: { date: Date; key: string; count: number }[] = [];
  for (let i = TREND_DAYS - 1; i >= 0; i--) {
    const d = new Date(today.getFullYear(), today.getMonth(), today.getDate() - i);
    const key = localDayKey(d);
    days.push({ date: d, key, count: byDay.get(key) ?? 0 });
  }
  const max = Math.max(1, ...days.map((d) => d.count));
  const total = days.reduce((sum, d) => sum + d.count, 0);
  const label = (d: Date) =>
    d.toLocaleDateString(undefined, { day: "numeric", month: "short" });

  return (
    <>
      <p className="card-note">
        {total === 0
          ? "No notes in this window"
          : `${total} note${total === 1 ? "" : "s"} · busiest day ${max}`}
      </p>
      <div className="trend">
        {days.map((d) => (
          <span
            key={d.key}
            className={d.count > 0 ? "trend-bar" : "trend-bar empty"}
            style={{
              height: d.count > 0 ? `${(d.count / max) * 100}%` : "2px",
            }}
            title={`${label(d.date)}: ${d.count} note${d.count === 1 ? "" : "s"}`}
          />
        ))}
      </div>
      <div className="trend-axis">
        <span>{days[0] ? label(days[0].date) : ""}</span>
        <span>today</span>
      </div>
    </>
  );
}
