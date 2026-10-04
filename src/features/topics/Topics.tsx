import { useCallback, useEffect, useState } from "react";
import {
  Citation,
  ConceptPage,
  ConceptSummary,
  getConcept,
  listConcepts,
} from "../../lib/api";
import TopicGraph from "./TopicGraph";

/**
 * Topics.
 *
 * A topic page shows the passages that mention it and nothing else — no
 * summary, no definition, no "this concept relates to". Every line of text on
 * screen is a note's own words, with the note it came from named beside it.
 * That isn't a stylistic choice: there is no generated text in the database to
 * show even if this wanted to.
 *
 * Which is also why the list can look thin at first. A topic appears once
 * something is written about it, and nowhere else.
 */
export default function Topics({
  onOpenNote,
}: {
  onOpenNote: (noteId: number) => void;
}) {
  const [concepts, setConcepts] = useState<ConceptSummary[]>([]);
  const [filter, setFilter] = useState("");
  const [open, setOpen] = useState<ConceptPage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [mode, setMode] = useState<"list" | "graph">("list");

  const refresh = useCallback(() => {
    listConcepts(filter.trim() || null, 200)
      .then((found) => {
        setConcepts(found);
        setError(null);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, [filter]);

  useEffect(refresh, [refresh]);

  function openTopic(conceptId: number) {
    getConcept(conceptId)
      .then(setOpen)
      .catch((e) => setError(String(e)));
  }

  if (open) {
    return (
      <TopicPage
        page={open}
        onBack={() => setOpen(null)}
        onOpenNote={onOpenNote}
      />
    );
  }

  return (
    <div className="topics">
      <header className="topics-header">
        <div>
          <h2>Topics</h2>
          <p className="card-note">
            Found in your notes, never invented. A topic is here because a note
            says it — open one to read the passages themselves.
          </p>
        </div>
        <div className="topics-controls">
          <div className="segmented">
            <button
              className={mode === "list" ? "active" : ""}
              onClick={() => setMode("list")}
            >
              List
            </button>
            <button
              className={mode === "graph" ? "active" : ""}
              onClick={() => setMode("graph")}
            >
              Graph
            </button>
          </div>
          {mode === "list" && (
            <input
              className="topics-filter"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              placeholder="Filter topics"
            />
          )}
        </div>
      </header>

      {mode === "graph" && (
        <TopicGraph onOpenTopic={openTopic} onOpenNote={onOpenNote} />
      )}

      {error && <p className="notes-error">{error}</p>}

      {mode === "list" && !loading && concepts.length === 0 && (
        <p className="empty">
          {filter.trim()
            ? `No topic matches “${filter.trim()}”.`
            : "No topics yet. They appear as notes are read — a note has to be saved first, and the worker reads it in the background."}
        </p>
      )}

      {mode === "list" && (
        <ul className="topic-list">
          {concepts.map((concept) => (
            <li key={concept.concept_id}>
              <button className="topic-chip" onClick={() => openTopic(concept.concept_id)}>
                <span className="topic-label">{concept.label}</span>
                <span className="topic-count">
                  {concept.notes} note{concept.notes === 1 ? "" : "s"}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function TopicPage({
  page,
  onBack,
  onOpenNote,
}: {
  page: ConceptPage;
  onBack: () => void;
  onOpenNote: (noteId: number) => void;
}) {
  return (
    <div className="topic-page">
      <button className="link-btn" onClick={onBack}>
        ‹ all topics
      </button>
      <h2>{page.label}</h2>
      <p className="card-note">
        {page.mentions} mention{page.mentions === 1 ? "" : "s"} across{" "}
        {page.notes} note{page.notes === 1 ? "" : "s"}. Every passage below is
        quoted from the note it names.
      </p>

      <ul className="citation-list">
        {page.citations.map((cited, i) => (
          <li key={i} className="citation">
            <CitedSnippet cited={cited} />
            <div className="citation-source">
              <button className="link-btn" onClick={() => onOpenNote(cited.note_id)}>
                {cited.note_title ?? `Note ${cited.note_id}`}
              </button>
              {cited.speaker && <span> · {cited.speaker}</span>}
              {cited.date && <span> · {cited.date}</span>}
              <span className="citation-engine"> · read by {cited.extracted_by}</span>
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

/**
 * The passage, with the mentioned words marked.
 *
 * Split by the offsets the database gave us rather than by searching for the
 * term: the offsets are the record of where it was found, and marking a
 * different occurrence would quietly misreport the evidence.
 */
function CitedSnippet({ cited }: { cited: Citation }) {
  const before = cited.snippet.slice(0, cited.char_start);
  const marked = cited.snippet.slice(cited.char_start, cited.char_end);
  const after = cited.snippet.slice(cited.char_end);
  return (
    <blockquote className="citation-text">
      {before}
      <mark>{marked}</mark>
      {after}
    </blockquote>
  );
}
