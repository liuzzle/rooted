import { invoke } from "@tauri-apps/api/core";

export interface Translation {
  id: number;
  abbrev: string;
  name: string;
  language: string;
}

export interface Book {
  osis: string;
  name: string;
  testament: "OT" | "NT";
  canonical_order: number;
  chapter_count: number;
}

export interface Token {
  idx: number;
  surface: string;
  char_start: number;
  char_end: number;
}

export interface Verse {
  verse_id: string;
  verse: number;
  text: string;
  tokens: Token[];
}

export function listTranslations(): Promise<Translation[]> {
  return invoke("list_translations");
}

export function listBooks(translationId: number): Promise<Book[]> {
  return invoke("list_books", { translationId });
}

export function getChapter(
  translationId: number,
  bookOsis: string,
  chapter: number,
): Promise<Verse[]> {
  return invoke("get_chapter", { translationId, bookOsis, chapter });
}

// --- notes & highlights ----------------------------------------------------

/**
 * Where a note or highlight lives. Verse anchors carry only `verse_id`, so they
 * stay valid across translations; word anchors add the translation, token index
 * and a `surface` snapshot of the word as it read when the note was written.
 */
export interface Anchor {
  anchor_type: "verse" | "word";
  verse_id: string;
  translation_id?: number | null;
  token_idx?: number | null;
  surface?: string | null;
}

export interface Note {
  note_id: number;
  title: string | null;
  body: string;
  created_at: string;
  updated_at: string;
  anchor_type: "verse" | "word";
  verse_id: string;
  translation_id: number | null;
  token_idx: number | null;
  surface: string | null;
}

export interface Highlight {
  id: number;
  anchor_type: "verse" | "word";
  verse_id: string;
  translation_id: number | null;
  token_idx: number | null;
  color: string;
}

export interface NoteMark {
  verse_id: string;
  token_idx: number | null; // null = verse-level note
  count: number;
}

export interface ChapterAnnotations {
  highlights: Highlight[];
  note_marks: NoteMark[];
}

export function verseAnchor(verseId: string): Anchor {
  return { anchor_type: "verse", verse_id: verseId };
}

export function wordAnchor(
  verseId: string,
  translationId: number,
  tokenIdx: number,
  surface: string,
): Anchor {
  return {
    anchor_type: "word",
    verse_id: verseId,
    translation_id: translationId,
    token_idx: tokenIdx,
    surface,
  };
}

export function anchorKey(anchor: Anchor): string {
  return anchor.anchor_type === "word"
    ? `w:${anchor.verse_id}:${anchor.token_idx}`
    : `v:${anchor.verse_id}`;
}

export function listNotes(anchor: Anchor): Promise<Note[]> {
  return invoke("list_notes", { anchor });
}

export function createNote(
  anchor: Anchor,
  title: string | null,
  body: string,
): Promise<number> {
  return invoke("create_note", { anchor, title, body });
}

export function updateNote(
  noteId: number,
  title: string | null,
  body: string,
): Promise<void> {
  return invoke("update_note", { noteId, title, body });
}

export function deleteNote(noteId: number): Promise<void> {
  return invoke("delete_note", { noteId });
}

export function setHighlight(anchor: Anchor, color: string): Promise<void> {
  return invoke("set_highlight", { anchor, color });
}

export function clearHighlight(anchor: Anchor): Promise<void> {
  return invoke("clear_highlight", { anchor });
}

export function getChapterAnnotations(
  translationId: number,
  bookOsis: string,
  chapter: number,
): Promise<ChapterAnnotations> {
  return invoke("get_chapter_annotations", { translationId, bookOsis, chapter });
}
