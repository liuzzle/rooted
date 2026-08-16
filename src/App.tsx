import { useCallback, useEffect, useState } from "react";
import {
  Anchor,
  Book,
  Translation,
  getActiveTranslation,
  listBooks,
  listTranslations,
  setActiveTranslation,
} from "./lib/api";
import Reader from "./features/reader/Reader";
import NotesLibrary from "./features/library/NotesLibrary";
import Dashboard from "./features/dashboard/Dashboard";
import TranslationsPanel from "./features/translations/TranslationsPanel";
import "./App.css";

type View = "read" | "notes" | "dashboard";

/** Where the reader should go when another view sends you somewhere. */
export interface ReadingTarget {
  bookOsis: string;
  chapter: number;
  selection: Anchor | null;
  /** Bumped on every jump so repeat jumps to the same place still register. */
  nonce: number;
}

export default function App() {
  const [view, setView] = useState<View>("read");
  const [translations, setTranslations] = useState<Translation[]>([]);
  const [translationId, setTranslationId] = useState<number | null>(null);
  const [books, setBooks] = useState<Book[]>([]);
  const [target, setTarget] = useState<ReadingTarget | null>(null);
  const [showPacks, setShowPacks] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /** Installed translations, settling on the remembered one if it's still here. */
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

  useEffect(() => {
    if (translationId == null) {
      setBooks([]);
      return;
    }
    listBooks(translationId)
      .then(setBooks)
      .catch((e) => setError(String(e)));
  }, [translationId]);

  function switchTranslation(id: number) {
    setTranslationId(id);
    const abbrev = translations.find((t) => t.id === id)?.abbrev;
    if (abbrev) setActiveTranslation(abbrev).catch((e) => setError(String(e)));
  }

  /** Send the reader somewhere and show it. */
  const jumpTo = useCallback(
    (bookOsis: string, chapter: number, selection: Anchor | null = null) => {
      setTarget({ bookOsis, chapter, selection, nonce: Date.now() });
      setView("read");
    },
    [],
  );

  const activeTranslation =
    translations.find((t) => t.id === translationId) ?? null;

  return (
    <div className="app">
      <div className="titlebar">
        <span className="brand">rooted</span>
        <nav className="views">
          {(
            [
              ["read", "Read"],
              ["notes", "Notes"],
              ["dashboard", "Dashboard"],
            ] as [View, string][]
          ).map(([id, label]) => (
            <button
              key={id}
              className={view === id ? "view-tab active" : "view-tab"}
              onClick={() => setView(id)}
            >
              {label}
            </button>
          ))}
        </nav>
        <div className="titlebar-right">
          {translations.length > 0 && (
            <select
              className="translation-select"
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
          )}
          <button className="ghost-btn light" onClick={() => setShowPacks(true)}>
            Translations…
          </button>
        </div>
      </div>

      {error && <div className="empty">Error: {error}</div>}

      {!error && loaded && translations.length === 0 && (
        <div className="empty">
          <h2>No Bible installed yet</h2>
          <p>Download a translation to start reading.</p>
          <button className="primary" onClick={() => setShowPacks(true)}>
            Browse translations
          </button>
        </div>
      )}

      {!error && translationId != null && (
        <>
          {view === "read" && (
            <Reader
              translationId={translationId}
              books={books}
              target={target}
              onError={setError}
            />
          )}
          {view === "notes" && (
            <NotesLibrary
              translationId={translationId}
              books={books}
              onJump={jumpTo}
            />
          )}
          {view === "dashboard" && (
            <Dashboard
              translationId={translationId}
              books={books}
              onJump={jumpTo}
            />
          )}
        </>
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
