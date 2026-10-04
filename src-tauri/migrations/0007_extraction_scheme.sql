-- Rooted — recording how a note was read (Phase 5)
--
-- `text_hash` catches a note that changed. It cannot catch the *rules* that
-- changed: improve the extractor and every note already indexed keeps the
-- concepts the old rules gave it, quietly, forever. So the scheme that produced
-- them is recorded, and a note read under an older one is read again.
--
-- `lang` is stored for the same reason it is detected at all: German inflection
-- and German stopwords are useless applied to an English note, and which rules
-- were used is part of knowing what the result means.

ALTER TABLE sources ADD COLUMN scheme TEXT;
ALTER TABLE sources ADD COLUMN lang TEXT;
