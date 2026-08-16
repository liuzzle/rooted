import { Book } from "./api";

/**
 * Parsing typed scripture references — "John 3:16", "1 Cor 13", "ps 23:1".
 *
 * Deliberately strict about the *book*: it only ever resolves to a book that
 * exists in the installed text, and returns null rather than guessing. Nothing
 * here invents a reference the user didn't type.
 */

export interface ParsedReference {
  book: Book;
  chapter: number;
  verse: number | null;
}

/** Common short forms people type that aren't a prefix of the full name. */
const ALIASES: Record<string, string> = {
  ps: "Ps",
  psalm: "Ps",
  psalms: "Ps",
  song: "Song",
  songs: "Song",
  sos: "Song",
  canticles: "Song",
  eccl: "Eccl",
  ecc: "Eccl",
  phil: "Phil",
  philemon: "Phlm",
  phlm: "Phlm",
  rev: "Rev",
  revelations: "Rev",
  acts: "Acts",
  jas: "Jas",
  james: "Jas",
};

function normalize(s: string): string {
  return s
    .toLowerCase()
    .replace(/[.’']/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

/** Leading ordinal in any of the forms people type: "1", "1st", "i", "first". */
function normalizeOrdinals(s: string): string {
  return s
    .replace(/^(1st|first)\s+/i, "1 ")
    .replace(/^(2nd|second)\s+/i, "2 ")
    .replace(/^(3rd|third)\s+/i, "3 ")
    .replace(/^i\s+/i, "1 ")
    .replace(/^ii\s+/i, "2 ")
    .replace(/^iii\s+/i, "3 ");
}

/**
 * Resolve a book name, code or abbreviation against the installed books.
 * Exact matches win over prefixes; an ambiguous prefix ("j", "1") resolves to
 * nothing rather than to a coin flip.
 */
export function matchBook(input: string, books: Book[]): Book | null {
  const query = normalize(normalizeOrdinals(input)).replace(/\s+/g, "");
  if (!query) return null;

  const aliased = ALIASES[query];
  if (aliased) {
    const hit = books.find((b) => b.osis.toLowerCase() === aliased.toLowerCase());
    if (hit) return hit;
  }

  const candidates = books.map((b) => ({
    book: b,
    osis: normalize(b.osis).replace(/\s+/g, ""),
    name: normalize(b.name).replace(/\s+/g, ""),
  }));

  const exact = candidates.find((c) => c.osis === query || c.name === query);
  if (exact) return exact.book;

  const prefixed = candidates.filter(
    (c) => c.name.startsWith(query) || c.osis.startsWith(query),
  );
  return prefixed.length === 1 ? prefixed[0].book : null;
}

/**
 * Parse a full reference. Returns null if the book can't be resolved
 * unambiguously or the chapter is out of range for that book.
 */
export function parseReference(
  input: string,
  books: Book[],
): ParsedReference | null {
  const trimmed = input.trim();
  if (!trimmed) return null;

  // Split "<book> <chapter>[:<verse>]" — the book may itself start with a digit.
  const match = trimmed.match(
    /^(.+?)[\s.]*(\d+)(?:\s*[:.\s]\s*(\d+))?\s*$/,
  );
  if (!match) {
    // Book alone ("John") — default to chapter 1.
    const book = matchBook(trimmed, books);
    return book ? { book, chapter: 1, verse: null } : null;
  }

  const [, bookPart, chapterPart, versePart] = match;
  const book = matchBook(bookPart, books);
  if (!book) return null;

  const chapter = Number(chapterPart);
  if (chapter < 1 || chapter > book.chapter_count) return null;

  const verse = versePart ? Number(versePart) : null;
  return { book, chapter, verse: verse && verse > 0 ? verse : null };
}

/** `"Gen.1"` — how a reading position is stored in settings. */
export function formatPosition(bookOsis: string, chapter: number): string {
  return `${bookOsis}.${chapter}`;
}

export function parsePosition(
  position: string | null,
): { bookOsis: string; chapter: number } | null {
  if (!position) return null;
  const [bookOsis, chapter] = position.split(".");
  const n = Number(chapter);
  return bookOsis && n > 0 ? { bookOsis, chapter: n } : null;
}

/** "Genesis 1:5" / "Genesis 1" for display. */
export function formatReference(
  bookName: string | null,
  bookOsis: string,
  chapter: number,
  verse?: number | null,
): string {
  const name = bookName ?? bookOsis;
  return verse ? `${name} ${chapter}:${verse}` : `${name} ${chapter}`;
}
