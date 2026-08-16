import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";

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
  /** Translation a word note was written in. */
  origin_abbrev: string | null;
  /** Word note shown at verse level because we're in another translation. */
  degraded: boolean;
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
  token_idx: number | null; // null = verse-level indicator
  count: number;
  /** Word notes from other translations, shown on the verse instead. */
  degraded: number;
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

/**
 * Notes at this anchor. `translationId` is the translation being read: word
 * notes made in a *different* one come back on the verse, flagged `degraded`.
 */
export function listNotes(
  anchor: Anchor,
  translationId: number,
): Promise<Note[]> {
  return invoke("list_notes", { anchor, translationId });
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

// --- translation packs -----------------------------------------------------

export interface Pack {
  abbrev: string;
  name: string;
  slug: string;
  language: string;
  license: string;
  year: string;
  versification: string;
  blurb: string;
  installed: boolean;
  translation_id: number | null;
  verse_count: number;
}

export interface PackProgress {
  abbrev: string;
  book: number;
  total: number;
  book_name: string;
  verses: number;
}

export function listPacks(): Promise<Pack[]> {
  return invoke("list_packs");
}

/** Downloads and imports a pack; emits `pack-progress` per book. */
export function installPack(abbrev: string): Promise<number> {
  return invoke("install_pack", { abbrev });
}

export function removePack(abbrev: string): Promise<void> {
  return invoke("remove_pack", { abbrev });
}

export function getActiveTranslation(): Promise<string | null> {
  return invoke("get_active_translation");
}

export function setActiveTranslation(abbrev: string): Promise<void> {
  return invoke("set_active_translation", { abbrev });
}

/** Subscribe to install progress. Resolves to an unlisten function. */
export function onPackProgress(
  handler: (p: PackProgress) => void,
): Promise<UnlistenFn> {
  return listen<PackProgress>("pack-progress", (e) => handler(e.payload));
}
