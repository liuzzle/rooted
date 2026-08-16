import { useCallback, useEffect, useState } from "react";
import {
  Anchor,
  Book,
  LibraryNote,
  createNote,
  deleteNote,
  listAllNotes,
  setNoteAnchor,
  updateNote,
  verseAnchor,
  wordAnchor,
} from "../../lib/api";
import { formatReference, parseReference } from "../../lib/reference";

/**
 * Every note in one list: anchored ones and standalone study notes.
 *
 * A note's reference can be attached, changed or removed here — but only by
 * typing a reference that actually resolves to an installed book, and word
 * anchors are never invented: attaching a reference always produces a verse
 * anchor.
 */
export default function NotesLibrary({
  translationId,
  books,
  onJump,
}: {
  translationId: number;
  books: Book[];
  onJump: (bookOsis: string, chapter: number, selection: Anchor | null) => void;
}) {
  const [notes, setNotes] = useState<LibraryNote[]>([]);
  const [bookFilter, setBookFilter] = useState<string>("");
  const [search, setSearch] = useState("");
  const [composing, setComposing] = useState(false);
  const [editing, setEditing] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    listAllNotes(translationId, bookFilter || null, search.trim() || null)
      .then(setNotes)
      .catch((e) => setError(String(e)));
  }, [translationId, bookFilter, search]);

  // Debounced so typing in the search box doesn't hit the DB per keystroke.
  useEffect(() => {
    const id = setTimeout(refresh, 150);
    return () => clearTimeout(id);
  }, [refresh]);

  function open(note: LibraryNote) {
    const a = note.anchor;
    if (!a) return;
    const selection =
      a.anchor_type === "word" && !a.degraded && a.translation_id != null
        ? wordAnchor(a.verse_id, a.translation_id, a.token_idx ?? 0, a.surface ?? "")
        : verseAnchor(a.verse_id);
    onJump(a.book_osis, a.chapter, selection);
  }

  async function remove(noteId: number) {
    try {
      await deleteNote(noteId);
      refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div className="library">
      <header className="library-header">
        <h2>Notes</h2>
        <div className="library-filters">
          <select
            value={bookFilter}
            onChange={(e) => setBookFilter(e.target.value)}
          >
            <option value="">All books</option>
            {books.map((b) => (
              <option key={b.osis} value={b.osis}>
                {b.name}
              </option>
            ))}
          </select>
          <input
            className="search"
            placeholder="Search notes…"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
          <button className="primary" onClick={() => setComposing((v) => !v)}>
            {composing ? "Cancel" : "+ New note"}
          </button>
        </div>
      </header>

      {error && <p className="notes-error">{error}</p>}

      {composing && (
        <NoteComposer
          books={books}
          onCancel={() => setComposing(false)}
          onSaved={() => {
            setComposing(false);
            refresh();
          }}
          onError={setError}
        />
      )}

      {notes.length === 0 && !composing && (
        <p className="notes-empty library-empty">
          {bookFilter || search
            ? "No notes match this filter."
            : "No notes yet. Write one here, or from any verse in the reader."}
        </p>
      )}

      <ul className="library-list">
        {notes.map((n) => (
          <li key={n.note_id} className="library-note">
            <div className="library-note-head">
              {n.anchor ? (
                <button className="ref-chip" onClick={() => open(n)}>
                  {formatReference(
                    n.anchor.book_name,
                    n.anchor.book_osis,
                    n.anchor.chapter,
                    n.anchor.verse,
                  )}
                  {n.anchor.anchor_type === "word" && (
                    <span className="ref-word"> · “{n.anchor.surface}”</span>
                  )}
                  {n.anchor.origin_abbrev && (
                    <span className="ref-origin"> {n.anchor.origin_abbrev}</span>
                  )}
                </button>
              ) : (
                <span className="ref-chip none">No reference</span>
              )}
              <span className="library-note-date">
                {n.created_at.slice(0, 10)}
              </span>
            </div>

            {editing === n.note_id ? (
              <NoteEditor
                note={n}
                books={books}
                onCancel={() => setEditing(null)}
                onSaved={() => {
                  setEditing(null);
                  refresh();
                }}
                onError={setError}
              />
            ) : (
              <>
                {n.title && <h4>{n.title}</h4>}
                <p className="note-body">{n.body}</p>
                <div className="note-actions">
                  {n.anchor && (
                    <button className="link-btn" onClick={() => open(n)}>
                      open in reader
                    </button>
                  )}
                  <button
                    className="link-btn"
                    onClick={() => setEditing(n.note_id)}
                  >
                    edit
                  </button>
                  <button className="link-btn" onClick={() => remove(n.note_id)}>
                    delete
                  </button>
                </div>
              </>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

/** New standalone note, with an optional reference typed as "John 3:16". */
function NoteComposer({
  books,
  onCancel,
  onSaved,
  onError,
}: {
  books: Book[];
  onCancel: () => void;
  onSaved: () => void;
  onError: (message: string) => void;
}) {
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  const [reference, setReference] = useState("");

  const parsed = parseReference(reference, books);
  const referenceInvalid = reference.trim().length > 0 && !parsed;

  async function save() {
    if (!body.trim() || referenceInvalid) return;
    try {
      const anchor = parsed
        ? verseAnchor(`${parsed.book.osis}.${parsed.chapter}.${parsed.verse ?? 1}`)
        : null;
      await createNote(anchor, title.trim() || null, body.trim());
      onSaved();
    } catch (e) {
      onError(String(e));
    }
  }

  return (
    <div className="note-form library-form">
      <input
        placeholder="Title (optional)"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
      />
      <textarea
        placeholder="Your note…"
        rows={5}
        autoFocus
        value={body}
        onChange={(e) => setBody(e.target.value)}
      />
      <ReferenceField
        value={reference}
        onChange={setReference}
        books={books}
        placeholder="Reference (optional) — e.g. John 3:16"
      />
      <div className="form-actions">
        <button
          className="primary"
          onClick={save}
          disabled={!body.trim() || referenceInvalid}
        >
          Save
        </button>
        <button className="link-btn" onClick={onCancel}>
          cancel
        </button>
      </div>
    </div>
  );
}

/** Edit a note's text, and attach / change / remove its reference. */
function NoteEditor({
  note,
  books,
  onCancel,
  onSaved,
  onError,
}: {
  note: LibraryNote;
  books: Book[];
  onCancel: () => void;
  onSaved: () => void;
  onError: (message: string) => void;
}) {
  const [title, setTitle] = useState(note.title ?? "");
  const [body, setBody] = useState(note.body);
  const [reference, setReference] = useState(
    note.anchor
      ? `${note.anchor.book_name ?? note.anchor.book_osis} ${note.anchor.chapter}:${note.anchor.verse}`
      : "",
  );

  const parsed = parseReference(reference, books);
  const referenceInvalid = reference.trim().length > 0 && !parsed;
  const isWordAnchor = note.anchor?.anchor_type === "word";

  async function save() {
    if (!body.trim() || referenceInvalid) return;
    try {
      await updateNote(note.note_id, title.trim() || null, body.trim());
      // A word anchor is left alone unless the reference was actually changed:
      // re-anchoring would drop the word it was written on.
      if (!isWordAnchor || reference.trim() === "") {
        const anchor = parsed
          ? verseAnchor(
              `${parsed.book.osis}.${parsed.chapter}.${parsed.verse ?? 1}`,
            )
          : null;
        const unchanged =
          (anchor?.verse_id ?? null) === (note.anchor?.verse_id ?? null);
        if (!unchanged) await setNoteAnchor(note.note_id, anchor);
      }
      onSaved();
    } catch (e) {
      onError(String(e));
    }
  }

  return (
    <div className="note-form">
      <input
        placeholder="Title (optional)"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
      />
      <textarea
        rows={5}
        autoFocus
        value={body}
        onChange={(e) => setBody(e.target.value)}
      />
      {isWordAnchor ? (
        <p className="field-hint">
          Anchored to the word “{note.anchor?.surface}” in{" "}
          {note.anchor?.origin_abbrev ?? "its translation"}. Clear the reference
          to detach it.
        </p>
      ) : null}
      <ReferenceField
        value={reference}
        onChange={setReference}
        books={books}
        placeholder="Reference — blank to detach"
      />
      <div className="form-actions">
        <button
          className="primary"
          onClick={save}
          disabled={!body.trim() || referenceInvalid}
        >
          Save
        </button>
        <button className="link-btn" onClick={onCancel}>
          cancel
        </button>
      </div>
    </div>
  );
}

/** Reference input that shows what it resolved to — or that it didn't. */
export function ReferenceField({
  value,
  onChange,
  books,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  books: Book[];
  placeholder: string;
}) {
  const parsed = parseReference(value, books);
  const invalid = value.trim().length > 0 && !parsed;
  return (
    <div className="reference-field">
      <input
        className={invalid ? "invalid" : ""}
        placeholder={placeholder}
        value={value}
        onChange={(e) => onChange(e.target.value)}
      />
      {parsed && (
        <span className="field-hint ok">
          → {formatReference(parsed.book.name, parsed.book.osis, parsed.chapter, parsed.verse)}
        </span>
      )}
      {invalid && <span className="field-hint bad">No such reference</span>}
    </div>
  );
}
