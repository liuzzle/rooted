#!/usr/bin/env python3
"""Finding the scripture a note cites — German first, English too."""
from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import references as r
import worker as w


class GermanReferenceTest(unittest.TestCase):
    def one(self, text: str) -> r.Reference:
        found = r.find_references(text)
        self.assertEqual(len(found), 1, f"expected one reference in {text!r}: {found}")
        return found[0]

    def test_the_german_comma_form(self):
        ref = self.one("Vergleiche Joh 3,16 dazu.")
        self.assertEqual((ref.book_osis, ref.chapter, ref.verse), ("John", 3, 16))
        self.assertEqual(ref.verse_id, "John.3.16")

    def test_an_ordinal_book_written_with_a_dot(self):
        ref = self.one("Der Bund in 1. Mose 12,1 beginnt.")
        self.assertEqual((ref.book_osis, ref.chapter, ref.verse), ("Gen", 12, 1))

    def test_ordinals_written_without_spaces(self):
        for written, osis in [("1Kor 13,4", "1Cor"), ("2Tim 3,16", "2Tim"),
                              ("1Joh 4,8", "1John")]:
            self.assertEqual(self.one(f"siehe {written}").book_osis, osis)

    def test_a_longer_name_wins_over_a_shorter_one(self):
        """"1. Johannes" must not be read as a stray 1 and "Johannes"."""
        ref = self.one("Siehe 1. Johannes 4,8.")
        self.assertEqual(ref.book_osis, "1John")

    def test_german_abbreviations_that_differ_from_english(self):
        self.assertEqual(self.one("Hes 36,26").book_osis, "Ezek")
        self.assertEqual(self.one("Offb 21,4").book_osis, "Rev")
        self.assertEqual(self.one("Spr 3,5").book_osis, "Prov")
        self.assertEqual(self.one("Pred 3,1").book_osis, "Eccl")
        self.assertEqual(self.one("Röm 8,28").book_osis, "Rom")

    def test_an_umlaut_free_spelling_still_resolves(self):
        """People type "Roemer" on keyboards without umlauts."""
        self.assertEqual(self.one("Roemer 8,28").book_osis, "Rom")

    def test_a_range_keeps_both_ends(self):
        ref = self.one("1. Mose 12,1-3")
        self.assertEqual((ref.verse, ref.verse_end), (1, 3))

    def test_a_chapter_on_its_own(self):
        ref = self.one("siehe Ps 23 insgesamt")
        self.assertEqual((ref.chapter, ref.verse), (23, None))
        self.assertIsNone(ref.verse_id, "a chapter is not a verse")

    def test_english_forms_still_work(self):
        self.assertEqual(self.one("see John 3:16").verse_id, "John.3.16")
        self.assertEqual(self.one("see 1 Cor 13").book_osis, "1Cor")

    def test_what_is_not_a_reference(self):
        for text in ["Kapitel 3,16 ist gemeint", "im Jahr 1912 geschrieben",
                     "Hesiod 3,4", "3,16 allein"]:
            self.assertEqual(r.find_references(text), [], text)

    def test_offsets_point_at_the_reference(self):
        text = "Der Bund (1. Mose 12,1-3) und Röm 4,3 gehören zusammen."
        for ref in r.find_references(text):
            self.assertEqual(text[ref.start:ref.end], ref.surface)

    def test_several_references_in_one_paragraph(self):
        found = r.find_references("Joh 3,16 und Röm 4,3 und Ps 23,1")
        self.assertEqual([f.book_osis for f in found], ["John", "Rom", "Ps"])

    # The next ones are written the way the real notes write them.

    def ids(self, text: str) -> list[str]:
        return [f.verse_id or f"{f.book_osis}.{f.chapter}" for f in r.find_references(text)]

    def test_a_verse_added_to_a_reference_is_found(self):
        self.assertEqual(self.ids("Hebräer 10, 5 - 10 + 14"), ["Heb.10.5", "Heb.10.14"])
        self.assertEqual(self.ids("Joh 3,16 und 18"), ["John.3.16", "John.3.18"])

    def test_a_chapter_added_to_a_reference_keeps_the_book(self):
        self.assertEqual(self.ids("Apg 2, 22-32 + 13,35"), ["Acts.2.22", "Acts.13.35"])
        self.assertEqual(self.ids("Röm 4,3; 5,1"), ["Rom.4.3", "Rom.5.1"])
        self.assertEqual(self.ids("Ps 23 + 24"), ["Ps.23", "Ps.24"])

    def test_a_continuation_is_marked_where_it_is_written(self):
        text = "Apg 2, 22-32 + 13,35"
        added = r.find_references(text)[1]
        self.assertEqual(text[added.start:added.end], "13,35")

    def test_a_number_that_starts_new_words_is_not_a_continuation(self):
        self.assertEqual(self.ids("Joh 3,16 und 2 Kinder kamen"), ["John.3.16"])
        self.assertEqual(self.ids("Joh 3,16 + 2. Mose 1,1"), ["John.3.16", "Exod.1.1"])

    def test_a_numbered_list_is_not_an_ordinal(self):
        """ "2. Johannes 14, 15" in a numbered list is John 14 — 2 John has
        one chapter. Where the chapter exists, the ordinal reading stands."""
        self.assertEqual(self.ids("2. Johannes 14, 15 - 20"), ["John.14.15"])
        self.assertEqual(self.ids("3. Johannes 14, 2 - 6"), ["John.14.2"])
        self.assertEqual(self.ids("2. Johannes 1,5"), ["2John.1.5"])
        self.assertEqual(self.ids("4. 2. Thessalonicher 1, 6"), ["2Thess.1.6"])
        text = "2. Johannes 14, 15"
        ref = r.find_references(text)[0]
        self.assertEqual(text[ref.start:ref.end], "Johannes 14, 15")


class StoredReferenceTest(unittest.TestCase):
    """Through the worker: found once, positioned in the note."""

    NOTE = """Der Bund mit Abraham (1. Mose 12,1-3) ist der Anfang.

Paulus greift ihn in Röm 4,3 auf. Vergleiche Joh 3,16 und Ps 23."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.conn = w.connect(Path(self.tmp.name) / "test.db")
        w.apply_migrations(self.conn)
        self.conn.execute(
            "INSERT INTO notes (title, body) VALUES ('Bund', ?)", (self.NOTE,)
        )
        w.Worker(self.conn, lease=60).index_notes()

    def tearDown(self) -> None:
        self.conn.close()
        self.tmp.cleanup()

    def test_every_reference_is_stored_where_it_was_written(self):
        rows = self.conn.execute(
            """SELECT v.surface, ch.char_start + v.char_start AS at
                 FROM verse_links v JOIN chunks ch USING (chunk_id)
                ORDER BY at"""
        ).fetchall()

        self.assertEqual([row["surface"] for row in rows],
                         ["1. Mose 12,1-3", "Röm 4,3", "Joh 3,16", "Ps 23"])
        for row in rows:
            # Offsets are into the note body, which is what the UI renders.
            at = row["at"]
            self.assertEqual(self.NOTE[at:at + len(row["surface"])], row["surface"])

    def test_a_citation_is_not_mistaken_for_a_topic(self):
        """"Vergleiche Joh 3,16" must not make a topic out of "Joh"."""
        labels = [
            row["label"] for row in self.conn.execute("SELECT label FROM concepts")
        ]
        for label in labels:
            self.assertNotIn("Joh", label, f"{label!r} came out of a citation")
            self.assertNotIn("Mose", label, f"{label!r} came out of a citation")

    def test_re_reading_a_note_does_not_double_its_references(self):
        self.conn.execute("UPDATE sources SET text_hash = 'stale'")
        w.Worker(self.conn, lease=60).index_notes()
        count = self.conn.execute("SELECT COUNT(*) c FROM verse_links").fetchone()["c"]
        self.assertEqual(count, 4)


if __name__ == "__main__":
    unittest.main()
