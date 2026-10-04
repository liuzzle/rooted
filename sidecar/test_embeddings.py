#!/usr/bin/env python3
"""
Vector tests — stdlib only, no model server.

The model is replaced by a fake that turns a text into a fixed vector, so what
is tested is the bookkeeping: which passages get a vector, under which model,
in what order, and what happens when the text or the model changes underneath.
"""

from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

os.environ.setdefault("ROOTED_OLLAMA_HOST", "http://127.0.0.1:9")

import embeddings  # noqa: E402
import worker as w  # noqa: E402


def fake_embed(texts, model=None, **_):
    """Deterministic, distinct per text, unit length."""
    return [embeddings.normalise([len(t), sum(map(ord, t)) % 97, 1.0]) for t in texts]


class VectorMathTest(unittest.TestCase):
    def test_vectors_round_trip_through_storage(self) -> None:
        v = embeddings.normalise([3.0, 4.0])
        self.assertEqual(v, [0.6, 0.8])
        back = embeddings.unpack(embeddings.pack(v))
        self.assertAlmostEqual(back[0], 0.6, places=6)
        self.assertAlmostEqual(back[1], 0.8, places=6)

    def test_a_zero_vector_stays_zero(self) -> None:
        self.assertEqual(embeddings.normalise([0.0, 0.0]), [0.0, 0.0])

    def test_no_server_means_no_model(self) -> None:
        self.assertFalse(embeddings.model_available("bge-m3", host="http://127.0.0.1:9"))


class EmbedPendingTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.conn = w.connect(Path(self.tmp.name) / "test.db")
        w.apply_migrations(self.conn)
        self.worker = w.Worker(self.conn, lease=60)
        self.conn.execute(
            "INSERT INTO books (osis, canonical_order, name, testament)"
            " VALUES ('Gen', 1, 'Genesis', 'OT')"
        )
        for tid, abbrev in [(1, "WEB"), (2, "ELB71")]:
            self.conn.execute(
                "INSERT INTO translations (id, abbrev, name) VALUES (?, ?, ?)",
                (tid, abbrev, abbrev),
            )
            for verse in (1, 2):
                self.conn.execute(
                    """INSERT INTO verses (verse_id, translation_id, book_osis,
                                           chapter, verse, text, canonical_order)
                       VALUES (?, ?, 'Gen', 1, ?, ?, ?)""",
                    (f"Gen.1.{verse}", tid, verse, f"{abbrev} verse {verse}", verse),
                )
        self.ready = mock.patch.object(embeddings, "model_available", return_value=True)
        self.embed = mock.patch.object(embeddings, "embed", side_effect=fake_embed)
        self.ready.start()
        self.fake = self.embed.start()

    def tearDown(self) -> None:
        mock.patch.stopall()
        self.conn.close()
        self.tmp.cleanup()

    def note(self, body: str) -> int:
        note_id = self.conn.execute(
            "INSERT INTO notes (title, body) VALUES ('Study', ?)", (body,)
        ).lastrowid
        self.worker.index_notes()
        return note_id

    def test_note_passages_and_verses_get_a_vector_from_the_named_model(self) -> None:
        self.note("Der Bund mit Abraham.\n\nDie Gnade bleibt.")

        self.worker.embed_pending()

        chunks = self.conn.execute(
            "SELECT embedded_by, embedding FROM chunks"
        ).fetchall()
        self.assertEqual(len(chunks), 2)
        self.assertTrue(all(c["embedded_by"] == embeddings.EMBED_MODEL for c in chunks))
        self.assertTrue(all(len(c["embedding"]) == 3 * 4 for c in chunks))
        verses = self.conn.execute("SELECT COUNT(*) FROM verse_vectors").fetchone()[0]
        self.assertEqual(verses, 4)

    def test_notes_come_before_verses_and_the_open_translation_first(self) -> None:
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES ('active_translation', 'ELB71')"
        )
        self.note("Der Bund mit Abraham.")

        self.worker.embed_pending()

        batches = [call.args[0] for call in self.fake.call_args_list]
        self.assertEqual(batches[0], ["Der Bund mit Abraham."])
        self.assertTrue(batches[1][0].startswith("ELB71"))

    def test_the_app_is_told_which_model_made_the_vectors(self) -> None:
        self.worker.embed_pending()
        value = self.conn.execute(
            "SELECT value FROM settings WHERE key = 'embedding'"
        ).fetchone()[0]
        self.assertIn(embeddings.EMBED_MODEL, value)
        self.assertIn('"available": true', value)

    def test_an_edited_note_is_embedded_again(self) -> None:
        note_id = self.note("Der Bund mit Abraham.")
        self.worker.embed_pending()
        self.conn.execute(
            "UPDATE notes SET body = 'Der Bund mit Isaak.' WHERE note_id = ?", (note_id,)
        )
        self.worker.index_notes()

        missing = self.conn.execute(
            "SELECT COUNT(*) FROM chunks WHERE embedded_by IS NULL"
        ).fetchone()[0]
        self.assertEqual(missing, 1, "the new passage starts without a vector")
        self.worker.embed_pending()
        self.assertEqual(
            self.conn.execute(
                "SELECT COUNT(*) FROM chunks WHERE embedded_by IS NULL"
            ).fetchone()[0],
            0,
        )

    def test_another_model_replaces_the_old_vectors(self) -> None:
        self.worker.embed_pending()
        with mock.patch.object(embeddings, "EMBED_MODEL", "other-model"):
            self.worker._verses_idle_until = 0.0
            self.worker.embed_pending()
        models = {
            r[0] for r in self.conn.execute("SELECT DISTINCT model FROM verse_vectors")
        }
        self.assertEqual(models, {"other-model"})

    def test_removing_a_pack_takes_its_vectors(self) -> None:
        self.worker.embed_pending()
        self.conn.execute("DELETE FROM verses WHERE translation_id = 2")
        left = {
            r[0] for r in self.conn.execute("SELECT DISTINCT translation_id FROM verse_vectors")
        }
        self.assertEqual(left, {1})

    def test_a_server_that_goes_away_pauses_without_failing(self) -> None:
        self.fake.side_effect = embeddings.EmbeddingUnavailable("connection refused")

        self.assertEqual(self.worker.embed_pending(), 0)

        self.assertFalse(self.worker._embed_ready)
        self.assertEqual(
            self.conn.execute("SELECT COUNT(*) FROM verse_vectors").fetchone()[0], 0
        )

    def test_nothing_happens_without_a_model(self) -> None:
        with mock.patch.object(embeddings, "model_available", return_value=False):
            self.worker._embed_checked = -1e9
            self.assertEqual(self.worker.embed_pending(), 0)
        self.fake.assert_not_called()


class SearchIndexTest(unittest.TestCase):
    """The word index follows chunks whichever process writes them."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.conn = w.connect(Path(self.tmp.name) / "test.db")
        w.apply_migrations(self.conn)
        self.worker = w.Worker(self.conn, lease=60)

    def tearDown(self) -> None:
        self.conn.close()
        self.tmp.cleanup()

    def found(self, query: str) -> int:
        return self.conn.execute(
            "SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH ?", (query,)
        ).fetchone()[0]

    def test_a_rewritten_note_is_found_by_its_new_words_only(self) -> None:
        note_id = self.conn.execute(
            "INSERT INTO notes (title, body) VALUES ('Study', 'Die Gnade Gottes.')"
        ).lastrowid
        self.worker.index_notes()
        self.assertEqual(self.found("Gnade"), 1)

        self.conn.execute(
            "UPDATE notes SET body = 'Der Bund Gottes.' WHERE note_id = ?", (note_id,)
        )
        self.worker.index_notes()

        self.assertEqual(self.found("Gnade"), 0)
        self.assertEqual(self.found("Bund"), 1)


if __name__ == "__main__":
    unittest.main()
