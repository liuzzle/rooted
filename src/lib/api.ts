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
