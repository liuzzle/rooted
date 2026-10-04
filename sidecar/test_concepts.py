#!/usr/bin/env python3
"""
Concept extraction tests — stdlib only.

The one that matters most is `test_a_paraphrase_is_rejected`: it is the
executable form of the promise this app makes. Everything else here exists to
make sure that check can't be bypassed by accident.
"""
from __future__ import annotations

import hashlib
import os
import sqlite3
import tempfile
import unittest
from pathlib import Path

# Hermetic: no test reaches a real model server, whatever is running on this
# machine. A closed port refuses at once. Export the variable to opt back in.
os.environ.setdefault("ROOTED_OLLAMA_HOST", "http://127.0.0.1:9")

import concepts  # noqa: E402
import worker as w  # noqa: E402

NOTE = """Covenant with Abraham is where this begins. The promise is repeated
to Isaac, and again to Jacob.

The covenant is not earned. Paul makes this point in Galatians: the promise
came before the law.

The covenant keeps being renewed without ever being deserved. Grace is the
word for that, though Paul does not use it here."""


class GateTest(unittest.TestCase):
    """Extraction is span-selection. Anything else is rejected."""

    def test_a_paraphrase_is_rejected(self):
        """The synthetic non-verbatim extraction the plan asks for.

        A model reading "the grace of God" and reporting "God's grace" has
        understood the note correctly and still may not be believed: the phrase
        it reports was never written, so there is no span to cite and nothing
        to store.
        """
        text = "What Paul describes here is the grace of God, unearned."
        with self.assertRaises(concepts.ExtractionRejected) as caught:
            concepts.gate(text, "God's grace", "test")
        self.assertIn("not in the text", str(caught.exception))

    def test_an_invented_term_is_rejected(self):
        text = "The promise came before the law."
        for invented in ["covenant theology", "dispensationalism", "grace"]:
            with self.assertRaises(concepts.ExtractionRejected):
                concepts.gate(text, invented, "test")

    def test_a_surviving_label_is_stored_as_the_text_wrote_it(self):
        """The stored surface comes from the note, never from the proposal."""
        text = "Covenant is the thread; the covenant is never earned."
        mentions = concepts.gate(text, "COVENANT", "test")

        self.assertEqual([m.surface for m in mentions], ["Covenant", "covenant"])
        for mention in mentions:
            self.assertEqual(text[mention.start:mention.end], mention.surface)

    def test_a_word_inside_another_word_is_not_a_mention(self):
        self.assertEqual(concepts.locate("his disgrace was public", "grace"), [])
        self.assertEqual(len(concepts.locate("grace upon grace", "grace")), 2)

    def test_a_label_may_span_a_line_break(self):
        """Notes wrap; a phrase broken across lines is still that phrase."""
        text = "the everlasting\ncovenant holds"
        found = concepts.locate(text, "everlasting covenant")
        self.assertEqual(len(found), 1)
        self.assertEqual(text[found[0][0]:found[0][1]], "everlasting\ncovenant")

    def test_a_batch_keeps_the_good_and_reports_the_bad(self):
        text = "Abraham believed God."
        kept, rejected = concepts.gather(text, ["Abraham", "faith alone"], "test")
        self.assertEqual([m.surface for m in kept], ["Abraham"])
        self.assertEqual(rejected, ["faith alone"])

    def test_absence_from_a_paragraph_is_not_a_rejection(self):
        """Asking which paragraphs contain a known topic is a different
        question from asking whether a proposal was honest."""
        self.assertEqual(concepts.locate_mentions("nothing here", "Abraham", "t"), [])


class KeyTest(unittest.TestCase):
    def test_the_same_string_written_normally_is_one_key(self):
        self.assertEqual(concepts.key_of("Covenant"), concepts.key_of("covenant"))
        self.assertEqual(concepts.key_of("new  covenant"), concepts.key_of("New Covenant"))

    def test_an_inflected_form_is_the_same_topic(self):
        """German declines; "Herr" and "Herrn" are one word being written
        normally, and must not be two topics."""
        self.assertEqual(concepts.key_of("Herr", "de"), concepts.key_of("Herrn", "de"))
        self.assertEqual(concepts.key_of("Gnade", "de"), concepts.key_of("Gnaden", "de"))
        self.assertEqual(concepts.key_of("Glaube", "de"), concepts.key_of("Glauben", "de"))
        self.assertEqual(concepts.key_of("covenant", "en"), concepts.key_of("covenants", "en"))

    def test_different_words_stay_different(self):
        """Normalising must not over-merge: these are not the same word."""
        self.assertNotEqual(concepts.key_of("Herr", "de"), concepts.key_of("Herz", "de"))
        self.assertNotEqual(concepts.key_of("Stein", "de"), concepts.key_of("Stern", "de"))
        self.assertNotEqual(concepts.key_of("sein", "de"), concepts.key_of("Segen", "de"))

    def test_the_key_is_still_not_a_synonym_test(self):
        """Merging these needs embeddings and a person, not morphology."""
        self.assertNotEqual(
            concepts.key_of("Gnade", "de"), concepts.key_of("unverdiente Güte", "de")
        )
        self.assertNotEqual(concepts.key_of("grace", "en"), concepts.key_of("mercy", "en"))


class DeterministicTest(unittest.TestCase):
    def test_a_name_mid_sentence_is_a_topic(self):
        labels = concepts.deterministic_labels("The promise came to Abraham first.")
        self.assertIn("Abraham", labels)

    def test_a_capital_that_is_only_grammar_is_not_a_topic(self):
        """"Later" opening a sentence is punctuation, not a name."""
        labels = concepts.deterministic_labels("Later the law came. Later still, grace.")
        self.assertNotIn("Later", labels)

    def test_a_word_the_note_keeps_returning_to_is_a_topic(self):
        labels = concepts.deterministic_labels(NOTE)
        self.assertIn("covenant", [label.casefold() for label in labels])

    def test_a_word_said_once_is_not(self):
        labels = concepts.deterministic_labels("the law came later, and mattered")
        self.assertNotIn("mattered", labels)

    def test_every_label_is_verbatim_in_the_note(self):
        """Belt and braces: the baseline slices text, so this can only fail if
        someone starts generating labels here."""
        for label in concepts.deterministic_labels(NOTE):
            self.assertTrue(concepts.locate(NOTE, label), f"{label!r} is not in the note")


GERMAN_NOTE = """Der Herr ist mein Hirte. Der Bund mit Abraham beginnt hier, und
die Verheißung wird an Isaak wiederholt.

Der Bund ist nicht verdient; das ist Gnade. Paulus sagt in Galater, dass die
Verheißung vor dem Gesetz kam. Dem Herrn sei Dank.

Der Bund wird immer wieder erneuert, ohne dass er verdient wäre. Gnade ist das
Wort dafür. Der Heilige Geist wirkt darin."""


class GermanTest(unittest.TestCase):
    """German is the corpus this is actually for, and it breaks the English
    assumptions: every noun is capitalised, and nouns decline."""

    def labels(self) -> list[str]:
        return concepts.deterministic_labels(GERMAN_NOTE)

    def test_the_language_is_recognised(self):
        self.assertEqual(concepts.detect_language(GERMAN_NOTE), "de")
        self.assertEqual(concepts.detect_language(NOTE), "en")

    def test_declined_forms_are_one_topic(self):
        """The note writes "Herr" and "Herrn"; that is one topic, and its label
        is the form the note uses most."""
        labels = self.labels()
        self.assertIn("Herr", labels)
        self.assertNotIn("Herrn", labels)

    def test_function_words_are_not_topics(self):
        """The ones that made the list before: they are grammar, not subjects."""
        labels = [label.casefold() for label in self.labels()]
        for word in ["nicht", "sein", "sich", "dass", "wird", "das", "der"]:
            self.assertNotIn(word, labels)

    def test_what_the_note_returns_to_is_a_topic(self):
        labels = self.labels()
        for expected in ["Bund", "Gnade", "Verheißung"]:
            self.assertIn(expected, labels)

    def test_a_noun_mentioned_once_is_not_a_topic(self):
        """In German a capital only means "noun". Nearly every sentence has
        several, so recurrence is what separates a subject from a passing word.
        """
        self.assertNotIn("Hirte", self.labels())

    def test_a_capitalised_phrase_stands_on_its_own(self):
        self.assertIn("Heilige Geist", self.labels())

    def test_every_label_is_verbatim_in_the_note(self):
        for label in self.labels():
            self.assertTrue(
                concepts.locate(GERMAN_NOTE, label), f"{label!r} is not in the note"
            )

    # Each of these is a line from a real note that used to come out as one
    # fused "topic".

    def phrases(self, text: str) -> list[str]:
        return [
            c.label
            for c in concepts.collect_candidates(text, "de").values()
            if c.words > 1
        ]

    def test_a_list_line_is_several_things_not_one(self):
        text = "vor 2000 Jahren\n\t- Ausharren, Bewahren, Hoffen"
        self.assertEqual(self.phrases(text), [])

    def test_separators_end_a_phrase(self):
        for text in ["Macht & Gerechtigkeit", "Christus / König / Sohn",
                     "Plan, Willen Gottes", "Opfer:\n\t\t- Lieblichkeiten"]:
            for phrase in self.phrases(text):
                self.assertNotIn(",", phrase)
                self.assertFalse({"Macht Gerechtigkeit", "Christus König Sohn",
                                  "Opfer Lieblichkeiten"} & {phrase}, text)
        self.assertEqual(self.phrases("Plan, Willen Gottes"), ["Willen Gottes"])

    def test_a_verse_marker_is_not_a_topic(self):
        text = "Tod (V9) -> im Tod (V10) -> Auferstehung (V11). V10 und V10."
        labels = concepts.deterministic_labels(text, "de")
        self.assertFalse([label for label in labels if any(ch.isdigit() for ch in label)])

    def test_a_phrase_is_words_with_only_spaces_between(self):
        self.assertIn("Heilige Geist", self.phrases("Der Heilige Geist kam."))
        # Each line of a list is its own item.
        self.assertEqual(self.phrases("Wohnungen\n\t\tGott"), [])

    def test_a_bullet_does_not_earn_a_capital(self):
        """After "- " a capital is layout, like after a full stop."""
        starts = concepts.sentence_starts("Notizen\n\t- Ausharren")
        self.assertIn("Notizen\n\t- Ausharren".index("Ausharren"), starts)

    def test_a_german_note_indexes_to_one_concept_per_word(self):
        """End to end: the declined forms must land on a single concept."""
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        conn = w.connect(Path(tmp.name) / "de.db")
        self.addCleanup(conn.close)
        w.apply_migrations(conn)
        conn.execute("INSERT INTO notes (title, body) VALUES ('Bund', ?)",
                     (GERMAN_NOTE,))
        w.Worker(conn, lease=60).index_notes()

        rows = conn.execute(
            """SELECT c.label, COUNT(*) n FROM concepts c
                 JOIN concept_mentions m USING (concept_id)
                GROUP BY c.concept_id ORDER BY n DESC"""
        ).fetchall()
        found = {r["label"]: r["n"] for r in rows}
        self.assertIn("Herr", found)
        self.assertEqual(found["Herr"], 2, "Herr and Herrn are one topic, mentioned twice")
        self.assertEqual(
            conn.execute("SELECT lang FROM sources").fetchone()["lang"], "de"
        )


class OllamaAnswerTest(unittest.TestCase):
    """Shape problems are forgiven; content problems are not."""

    def test_topics_are_read_from_the_answer(self):
        self.assertEqual(
            concepts.parse_ollama_topics('{"topics": ["covenant", "grace"]}'),
            ["covenant", "grace"],
        )

    def test_a_bare_list_is_accepted(self):
        self.assertEqual(concepts.parse_ollama_topics('["covenant"]'), ["covenant"])

    def test_junk_is_no_topics_rather_than_an_error(self):
        for junk in ["", "I think the topics are...", "{}", "null", '{"topics": [1, 2]}']:
            self.assertEqual(concepts.parse_ollama_topics(junk), [])


class ChunkTest(unittest.TestCase):
    def test_a_chunk_can_be_cut_back_out_of_the_note(self):
        """Offsets are into the note, so a citation can always be resolved."""
        for start, end, text in concepts.chunk_text(NOTE):
            self.assertEqual(NOTE[start:end], text)

    def test_paragraphs_survive_their_line_breaks(self):
        chunks = concepts.chunk_text(NOTE)
        self.assertEqual(len(chunks), 3)
        self.assertIn("\n", chunks[0][2], "a wrapped paragraph is still one chunk")


class IndexingTest(unittest.TestCase):
    """The ledger, end to end, through the worker."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.conn = w.connect(Path(self.tmp.name) / "test.db")
        w.apply_migrations(self.conn)
        self.worker = w.Worker(self.conn, lease=60)

    def tearDown(self) -> None:
        self.conn.close()
        self.tmp.cleanup()

    def add_note(self, body: str, title: str = "Study") -> int:
        cur = self.conn.execute(
            "INSERT INTO notes (title, body) VALUES (?, ?)", (title, body)
        )
        return cur.lastrowid

    def mentions(self) -> list[sqlite3.Row]:
        return self.conn.execute(
            """SELECT c.label, m.surface, m.char_start, m.char_end, m.extracted_by,
                      ch.text AS chunk_text
                 FROM concept_mentions m
                 JOIN concepts c ON c.concept_id = m.concept_id
                 JOIN chunks ch ON ch.chunk_id = m.chunk_id"""
        ).fetchall()

    def test_a_note_becomes_concepts_with_resolvable_citations(self):
        self.add_note(NOTE)
        self.assertEqual(self.worker.index_notes(), 1)

        found = self.mentions()
        self.assertTrue(found)
        for mention in found:
            # The invariant the whole design rests on: the stored surface is
            # still exactly what stands at those offsets.
            actual = mention["chunk_text"][mention["char_start"]:mention["char_end"]]
            self.assertEqual(actual, mention["surface"])

    def test_the_topic_a_note_keeps_returning_to_leads(self):
        self.add_note(NOTE)
        self.worker.index_notes()
        top = self.conn.execute(
            """SELECT c.label, COUNT(*) n
                 FROM concepts c JOIN concept_mentions m USING (concept_id)
                GROUP BY c.concept_id ORDER BY n DESC LIMIT 1"""
        ).fetchone()
        self.assertEqual(top["label"].casefold(), "covenant")

    def test_one_concept_for_one_word_however_it_is_written(self):
        self.add_note(NOTE)
        self.worker.index_notes()
        keys = [r["key"] for r in self.conn.execute("SELECT key FROM concepts")]
        self.assertEqual(len(keys), len(set(keys)))
        self.assertIn(concepts.key_of("covenant", "en"), keys)

    def merge(self, from_label: str, into_label: str) -> int:
        """What the app does when a person merges two topics."""
        row = lambda label: self.conn.execute(
            "SELECT concept_id, key, label FROM concepts WHERE label = ?", (label,)
        ).fetchone()
        src, dst = row(from_label), row(into_label)
        self.conn.execute(
            "UPDATE OR IGNORE concept_mentions SET concept_id = ? WHERE concept_id = ?",
            (dst["concept_id"], src["concept_id"]),
        )
        self.conn.execute(
            "INSERT INTO concept_aliases (key, concept_id, label) VALUES (?, ?, ?)",
            (src["key"], dst["concept_id"], src["label"]),
        )
        self.conn.execute("DELETE FROM concepts WHERE concept_id = ?", (src["concept_id"],))
        return dst["concept_id"]

    def test_a_merge_survives_the_note_being_read_again(self):
        """Re-reading must not quietly bring a merged topic back."""
        note_id = self.add_note(NOTE)
        self.worker.index_notes()
        labels = {r["label"] for r in self.conn.execute("SELECT label FROM concepts")}
        self.assertTrue({"Abraham", "Isaac"} <= labels, labels)
        survivor = self.merge("Isaac", "Abraham")

        self.conn.execute(
            "UPDATE notes SET body = body || ' Isaac again.' WHERE note_id = ?", (note_id,)
        )
        self.worker.index_notes()

        labels = {r["label"] for r in self.conn.execute("SELECT label FROM concepts")}
        self.assertNotIn("Isaac", labels)
        isaac = self.conn.execute(
            "SELECT concept_id FROM concept_mentions WHERE surface = 'Isaac'"
        ).fetchall()
        self.assertTrue(isaac)
        self.assertTrue(all(r["concept_id"] == survivor for r in isaac))

    def test_an_unread_note_is_read_once_and_then_left_alone(self):
        self.add_note(NOTE)
        self.assertEqual(self.worker.index_notes(), 1)
        self.assertEqual(self.worker.index_notes(), 0, "re-read an unchanged note")

    def test_better_rules_reach_notes_that_were_already_read(self):
        """Otherwise improving the extractor leaves old notes on old rules,
        quietly, forever."""
        self.add_note(NOTE)
        self.worker.index_notes()
        self.conn.execute("UPDATE sources SET scheme = 'phrases/1-ancient'")

        self.assertEqual(self.worker.index_notes(), 1)
        self.assertEqual(
            self.conn.execute("SELECT scheme FROM sources").fetchone()["scheme"],
            concepts.SCHEME,
        )

    def test_an_edited_note_is_read_again_and_leaves_nothing_stale(self):
        """Mentions cite offsets; editing the note moves them, so the old ones
        must not survive."""
        note_id = self.add_note(NOTE)
        self.worker.index_notes()
        self.assertTrue(
            self.conn.execute(
                "SELECT 1 FROM concepts WHERE key = 'abraham'"
            ).fetchone()
        )

        self.conn.execute(
            "UPDATE notes SET body = ? WHERE note_id = ?",
            ("A short note about Melchizedek, and nothing else.", note_id),
        )
        self.assertEqual(self.worker.index_notes(), 1)

        for mention in self.mentions():
            actual = mention["chunk_text"][mention["char_start"]:mention["char_end"]]
            self.assertEqual(actual, mention["surface"])
        surfaces = {m["surface"] for m in self.mentions()}
        self.assertNotIn("Abraham", surfaces, "a mention outlived the text it cited")

    def test_the_hash_recorded_is_the_text_that_was_read(self):
        note_id = self.add_note(NOTE)
        self.worker.index_notes()
        source = self.conn.execute(
            "SELECT text_hash, indexed_by FROM sources WHERE note_id = ?", (note_id,)
        ).fetchone()
        self.assertEqual(
            source["text_hash"], hashlib.sha256(NOTE.encode("utf-8")).hexdigest()
        )
        self.assertIn("phrases", source["indexed_by"])

    def test_an_empty_note_is_not_indexed(self):
        self.add_note("")
        self.assertEqual(self.worker.index_notes(), 0)


if __name__ == "__main__":
    unittest.main()
