import { useEffect, useState } from "react";
import {
  Anchor,
  Note,
  anchorKey,
  clearHighlight,
  createNote,
  deleteNote,
  listNotes,
  setHighlight,
  updateNote,
} from "../../lib/api";

/** Highlight palette. Values are CSS-class suffixes (see App.css `.hl-*`). */
export const HIGHLIGHT_COLORS = [
  "yellow",
  "green",
  "blue",
  "pink",
  "purple",
] as const;

export default function NotesPanel({
  anchor,
  translationId,
  label,
  highlight,
  onChanged,
  onClose,
}: {
  anchor: Anchor;
  /** Translation being read — decides which word notes are degraded. */
  translationId: number;
  label: string;
  highlight: string | null;
  onChanged: () => void;
  onClose: () => void;
}) {
  const [notes, setNotes] = useState<Note[]>([]);
  const [editing, setEditing] = useState<number | "new" | null>(null);
  const [draftTitle, setDraftTitle] = useState("");
  const [draftBody, setDraftBody] = useState("");
  const [error, setError] = useState<string | null>(null);

  const key = anchorKey(anchor);

  // Reload whenever the selected anchor or translation changes; reset any
  // in-progress draft.
  useEffect(() => {
    setEditing(null);
    setDraftTitle("");
    setDraftBody("");
    listNotes(anchor, translationId)
      .then(setNotes)
      .catch((e) => setError(String(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, translationId]);

  function refresh() {
    listNotes(anchor, translationId)
      .then(setNotes)
      .catch((e) => setError(String(e)));
    onChanged();
  }

  function startNew() {
    setEditing("new");
    setDraftTitle("");
    setDraftBody("");
  }

  function startEdit(note: Note) {
    setEditing(note.note_id);
    setDraftTitle(note.title ?? "");
    setDraftBody(note.body);
  }

  async function save() {
    const body = draftBody.trim();
    if (!body) return;
    const title = draftTitle.trim() || null;
    try {
      if (editing === "new") {
        await createNote(anchor, title, body);
      } else if (typeof editing === "number") {
        await updateNote(editing, title, body);
      }
      setEditing(null);
      setDraftTitle("");
      setDraftBody("");
      refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  async function remove(noteId: number) {
    try {
      await deleteNote(noteId);
      if (editing === noteId) setEditing(null);
      refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  async function paint(color: string | null) {
    try {
      if (color === null) await clearHighlight(anchor);
      else await setHighlight(anchor, color);
      onChanged();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <aside className="notes-panel">
      <header className="notes-header">
        <div>
          <span className="notes-kind">
            {anchor.anchor_type === "word" ? "Word" : "Verse"}
          </span>
          <h3>{label}</h3>
        </div>
        <button className="icon-btn" onClick={onClose} title="Close">
          ×
        </button>
      </header>

      <div className="highlight-row">
        <span className="field-label">Highlight</span>
        <div className="swatches">
          {HIGHLIGHT_COLORS.map((c) => (
            <button
              key={c}
              className={`swatch hl-${c}${highlight === c ? " active" : ""}`}
              title={c}
              onClick={() => paint(highlight === c ? null : c)}
            />
          ))}
          <button
            className={`swatch none${highlight === null ? " active" : ""}`}
            title="No highlight"
            onClick={() => paint(null)}
          />
        </div>
      </div>

      {error && <p className="notes-error">{error}</p>}

      <div className="notes-list">
        {notes.length === 0 && editing !== "new" && (
          <p className="notes-empty">No notes here yet.</p>
        )}

        {notes.map((n) =>
          editing === n.note_id ? (
            <NoteForm
              key={n.note_id}
              title={draftTitle}
              body={draftBody}
              onTitle={setDraftTitle}
              onBody={setDraftBody}
              onSave={save}
              onCancel={() => setEditing(null)}
            />
          ) : (
            <article
              key={n.note_id}
              className={n.degraded ? "note degraded" : "note"}
            >
              {n.degraded && (
                <p className="degraded-label">
                  originally on the word “{n.surface}”
                  {n.origin_abbrev ? ` in ${n.origin_abbrev}` : ""}
                </p>
              )}
              {n.title && <h4>{n.title}</h4>}
              <p className="note-body">{n.body}</p>
              <div className="note-meta">
                <span>{n.updated_at}</span>
                <span className="note-actions">
                  <button className="link-btn" onClick={() => startEdit(n)}>
                    edit
                  </button>
                  <button className="link-btn" onClick={() => remove(n.note_id)}>
                    delete
                  </button>
                </span>
              </div>
            </article>
          ),
        )}

        {editing === "new" && (
          <NoteForm
            title={draftTitle}
            body={draftBody}
            onTitle={setDraftTitle}
            onBody={setDraftBody}
            onSave={save}
            onCancel={() => setEditing(null)}
          />
        )}
      </div>

      {editing !== "new" && (
        <button className="add-note" onClick={startNew}>
          + Add note
        </button>
      )}
    </aside>
  );
}

function NoteForm({
  title,
  body,
  onTitle,
  onBody,
  onSave,
  onCancel,
}: {
  title: string;
  body: string;
  onTitle: (v: string) => void;
  onBody: (v: string) => void;
  onSave: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="note-form">
      <input
        placeholder="Title (optional)"
        value={title}
        onChange={(e) => onTitle(e.target.value)}
      />
      <textarea
        placeholder="Your note…"
        rows={5}
        autoFocus
        value={body}
        onChange={(e) => onBody(e.target.value)}
      />
      <div className="form-actions">
        <button className="primary" onClick={onSave} disabled={!body.trim()}>
          Save
        </button>
        <button className="link-btn" onClick={onCancel}>
          cancel
        </button>
      </div>
    </div>
  );
}
