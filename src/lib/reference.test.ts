import { describe, expect, it } from "vitest";
import { Book } from "./api";
import {
  formatPosition,
  formatReference,
  matchBook,
  parsePosition,
  parseReference,
} from "./reference";

const book = (
  osis: string,
  name: string,
  chapter_count: number,
  testament: "OT" | "NT" = "OT",
): Book => ({ osis, name, testament, canonical_order: 0, chapter_count });

const BOOKS: Book[] = [
  book("Gen", "Genesis", 50),
  book("Exod", "Exodus", 40),
  book("Ps", "Psalms", 150),
  book("Song", "Song of Solomon", 8),
  book("Eccl", "Ecclesiastes", 12),
  book("Isa", "Isaiah", 66),
  book("John", "John", 21, "NT"),
  book("1John", "1 John", 5, "NT"),
  book("2John", "2 John", 1, "NT"),
  book("1Cor", "1 Corinthians", 16, "NT"),
  book("2Cor", "2 Corinthians", 13, "NT"),
  book("Jas", "James", 5, "NT"),
  book("Phlm", "Philemon", 1, "NT"),
  book("Phil", "Philippians", 4, "NT"),
  book("Rev", "Revelation", 22, "NT"),
];

/** "John 3:16" -> "John 3:16"; unparsed -> null. */
function ref(input: string): string | null {
  const parsed = parseReference(input, BOOKS);
  return parsed
    ? `${parsed.book.osis} ${parsed.chapter}:${parsed.verse ?? "-"}`
    : null;
}

describe("parseReference", () => {
  it.each([
    ["John 3:16", "John 3:16"],
    ["john 3:16", "John 3:16"],
    ["  John   3 : 16  ", "John 3:16"],
    ["gen1:1", "Gen 1:1"],
    ["Isaiah 40.31", "Isa 40:31"],
    ["Genesis 1", "Gen 1:-"],
    ["John", "John 1:-"],
  ])("parses %j", (input, expected) => {
    expect(ref(input)).toBe(expected);
  });

  it.each([
    ["1 Cor 13", "1Cor 13:-"],
    ["1Cor 13:4", "1Cor 13:4"],
    ["1 John 4:8", "1John 4:8"],
    ["i john 4:8", "1John 4:8"],
    ["first john 4:8", "1John 4:8"],
    ["2 John 1", "2John 1:-"],
  ])("handles the numbered book %j", (input, expected) => {
    expect(ref(input)).toBe(expected);
  });

  it.each([
    ["psalm 23", "Ps 23:-"],
    ["psalms 23:1", "Ps 23:1"],
    ["Ps 23:1", "Ps 23:1"],
    ["James 1:5", "Jas 1:5"],
    ["Song 2:1", "Song 2:1"],
  ])("resolves the alias %j", (input, expected) => {
    expect(ref(input)).toBe(expected);
  });

  it("keeps Philippians and Philemon apart", () => {
    expect(ref("Phil 4:13")).toBe("Phil 4:13");
    expect(ref("Philemon 1")).toBe("Phlm 1:-");
  });

  // Refusing to guess is the point: a wrong reference would file a note against
  // scripture the user never chose.
  it.each([
    ["", "empty"],
    ["12345", "no book at all"],
    ["Hezekiah 3:1", "no such book"],
    ["J 3:16", "ambiguous — John, 1 John, 2 John, James"],
    ["Gen 51", "chapter past the end of the book"],
    ["Ps 0", "chapter zero"],
  ])("refuses %j (%s)", (input) => {
    expect(ref(input)).toBeNull();
  });

  it("only resolves books that are actually installed", () => {
    expect(parseReference("Genesis 1", [])).toBeNull();
    expect(matchBook("Exodus", BOOKS)?.osis).toBe("Exod");
    expect(matchBook("Exodus", [BOOKS[0]])).toBeNull();
  });
});

describe("reading positions", () => {
  it("round-trips", () => {
    expect(formatPosition("Gen", 1)).toBe("Gen.1");
    expect(parsePosition("Gen.1")).toEqual({ bookOsis: "Gen", chapter: 1 });
  });

  it("rejects nothing-to-parse", () => {
    expect(parsePosition(null)).toBeNull();
    expect(parsePosition("")).toBeNull();
    expect(parsePosition("Gen.zero")).toBeNull();
  });
});

describe("formatReference", () => {
  it("falls back to the OSIS code when a book name is missing", () => {
    expect(formatReference("Genesis", "Gen", 1, 5)).toBe("Genesis 1:5");
    expect(formatReference(null, "Gen", 1, 5)).toBe("Gen 1:5");
    expect(formatReference("Genesis", "Gen", 1, null)).toBe("Genesis 1");
  });
});
