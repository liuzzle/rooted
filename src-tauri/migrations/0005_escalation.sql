-- Rooted — cloud escalation for scans (Phase 4)
--
-- On-device OCR is the default and always runs first. Some pages it cannot
-- read: faint pencil, heavy cursive, a hand it has no chance with. For those,
-- and only when a person asks for it page by page, the lines can be re-read by
-- a stronger model that is not on this machine.
--
-- That is the only path by which anything in this app leaves the computer, so
-- it is a stored, per-job decision rather than a setting: `escalate` is set by
-- an explicit action in the UI, and cleared as soon as the worker has acted on
-- it. Nothing re-escalates on its own, and a retry never inherits it.
--
-- What is sent is the cropped line images, not the note, not the metadata, and
-- not any other document. What comes back is a reading of those lines, which
-- is still text a person has to accept in review before it becomes a note.

ALTER TABLE jobs ADD COLUMN escalate INTEGER NOT NULL DEFAULT 0;
