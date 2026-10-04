#!/usr/bin/env python3
"""
Concept extraction: which topics a note actually talks about.

The rule this module exists to enforce: **extraction is span-selection, not
generation.** An extractor proposes a piece of text it thinks is a topic; this
module checks that the piece is verbatim present in the note and records where.
Anything that isn't found in the text — a tidied-up label, a synonym, a plural
made singular, an invented term — is rejected. Not down-ranked, not stored with
a low score: rejected, because a topic that isn't in the sources is exactly the
thing this app promises never to show.

That is why extractors return *strings*, never offsets. A model asked for
character positions will produce plausible ones; asking only "what did you see"
and locating it ourselves means the offsets are ours, computed from the text,
and can't be wrong about where a word is. It also means the gate is the same
code for every extractor — the deterministic one has nothing to prove either.
"""
from __future__ import annotations

import json
import os
import re
import unicodedata
from dataclasses import dataclass
from functools import lru_cache
from typing import Iterable, Optional

# Words that are never a topic on their own, by language. Checked against the
# *lemma*, so every inflected form of a function word is covered by its base
# form — "seinen" and "seine" are caught by "sein".
#
# Deliberately function words only. A longer list would start deciding what
# counts as theology, which is not this file's business.
STOPWORDS = {
    "en": {
        "a", "about", "after", "all", "also", "an", "and", "any", "are", "as",
        "at", "be", "because", "been", "before", "but", "by", "can", "did",
        "do", "does", "for", "from", "had", "has", "have", "he", "her", "here",
        "him", "his", "how", "i", "if", "in", "into", "is", "it", "its", "may",
        "me", "more", "must", "my", "no", "not", "now", "of", "on", "one",
        "only", "or", "other", "our", "out", "over", "own", "said", "same",
        "see", "she", "should", "so", "some", "such", "than", "that", "the",
        "their", "them", "then", "there", "these", "they", "this", "those",
        "through", "to", "too", "under", "up", "was", "we", "were", "what",
        "when", "where", "which", "while", "who", "why", "will", "with",
        "would", "you", "your",
    },
    "de": {
        "aber", "all", "alle", "allem", "allen", "aller", "alles", "als",
        "also", "am", "an", "ander", "andere", "anderen", "auch", "auf", "aus",
        "bei", "beim", "bin", "bis", "bist", "da", "damit", "dann", "das",
        "dass", "daß", "dein", "dem", "den", "denen", "denn", "der", "deren",
        "des", "dessen", "dich", "die", "dies", "diese", "diesem", "diesen",
        "dieser", "dieses", "dir", "doch", "dort", "du", "durch", "ein",
        "eine", "einem", "einen", "einer", "eines", "er", "es", "etwas",
        "euch", "euer", "für", "gegen", "gewesen", "haben", "hat", "hatte",
        "hatten", "hier", "hin", "hinter", "ich", "ihm", "ihn", "ihnen", "ihr",
        "im", "in", "ins", "ist", "ja", "jede", "jedem", "jeden", "jeder",
        "jedes", "jene", "jetzt", "kann", "kein", "keine", "können", "könnte",
        "machen", "man", "mehr", "mein", "meine", "mich", "mir", "mit",
        "muss", "müssen", "nach", "nicht", "nichts", "noch", "nun", "nur",
        "ob", "oder", "ohne", "schon", "sehr", "sein", "seine", "selbst",
        "sich", "sie", "sind", "so", "solche", "soll", "sollen", "sollte",
        "sondern", "sonst", "über", "um", "und", "uns", "unser", "unter",
        "viel", "vom", "von", "vor", "während", "war", "waren", "was", "weg",
        "weil", "weiter", "welche", "wenn", "werden", "wie", "wieder",
        "will", "wir", "wird", "wo", "wollen", "wollte", "würde", "würden",
        "zu", "zum", "zur", "zwar", "zwischen",
    },
}

# Which languages the extractor knows. Detection picks between them per note.
LANGUAGES = tuple(STOPWORDS)
DEFAULT_LANGUAGE = "de"

# Bumped whenever the key or the selection rules change, so notes indexed under
# the old rules are read again instead of sitting there with stale concepts.
SCHEME = "phrases/3-lemma"

# A word, keeping its position. Apostrophes and hyphens stay inside a word so
# "God's" and "self-control" survive as written.
WORD = re.compile(r"[^\W\d_][\w'’-]*", re.UNICODE)

# How often a lowercase term has to recur in one note before it counts as
# something the note is *about* rather than something it merely says.
REPEAT_THRESHOLD = 3

# Longest candidate, in words. Beyond this it's a sentence, not a topic.
MAX_WORDS = 4


@dataclass(frozen=True)
class Mention:
    """A concept found at an exact place in a piece of text.

    `surface` is sliced out of the source at `start`/`end`, never copied from
    what an extractor proposed — so it is always what the note actually says.
    """

    surface: str
    start: int
    end: int
    extracted_by: str
    confidence: Optional[float] = None


class ExtractionRejected(Exception):
    """A proposed concept was not verbatim in the source. It is dropped."""


# ---------------------------------------------------------------------------
# The gate
# ---------------------------------------------------------------------------

def locate(text: str, label: str,
           lang: str = DEFAULT_LANGUAGE) -> list[tuple[int, int]]:
    """Every place `label` occurs in `text`, in any form of the same words.

    Matching is by word, on the normalised form — so a topic recorded as "Herr"
    finds "Herrn", and one recorded as "covenant" finds "covenants". Matching
    the literal string instead would record a topic and then fail to find most
    of its mentions, which is how a graph ends up understating what a note says.

    Two properties this keeps, which a looser matcher would lose:

    - **Word sequences, not substrings.** "grace" is not found inside
      "disgrace", and "God's grace" does not match "the grace of God" — the
      words have to be adjacent, in order.
    - **Offsets are real.** The range returned spans actual words in the text,
      so slicing it yields what was written, inflection and capitalisation
      included.
    """
    target = [normalise_word(w, lang) for w, _, _ in words_with_positions(label)]
    if not target:
        return []
    words = words_with_positions(text)
    keys = [normalise_word(w, lang) for w, _, _ in words]

    found: list[tuple[int, int]] = []
    span = len(target)
    for i in range(len(words) - span + 1):
        if keys[i:i + span] == target:
            found.append((words[i][1], words[i + span - 1][2]))
    return found


def gate(text: str, label: str, extracted_by: str,
         confidence: Optional[float] = None,
         lang: str = DEFAULT_LANGUAGE) -> list[Mention]:
    """Turn a proposed label into mentions, or reject it.

    This is the only way a mention is ever made. The surface is sliced out of
    the text at the offsets found, so what gets stored is what the note says —
    if an extractor proposed "Gods grace" and the note reads "God's grace",
    there is no match and nothing is stored.

    A different *inflection* of the same words is not a paraphrase and is
    accepted; the surface stored is still the note's own. A different choice of
    words is a paraphrase, and is not.
    """
    found = locate(text, label, lang)
    if not found:
        raise ExtractionRejected(
            f"{extracted_by} proposed {label!r}, which is not in the text"
        )
    return [
        Mention(text[start:end], start, end, extracted_by, confidence)
        for start, end in found
    ]


def locate_mentions(text: str, label: str, extracted_by: str,
                    confidence: Optional[float] = None,
                    lang: str = DEFAULT_LANGUAGE,
                    skip: Optional[list[tuple[int, int]]] = None) -> list[Mention]:
    """Mentions of `label` in `text`, or none. The forgiving form of the gate.

    Used when a label is already known to be in the note and we are asking
    which paragraphs contain it: not finding it here says nothing about the
    label's honesty, so there is nothing to reject.
    """
    try:
        found = gate(text, label, extracted_by, confidence, lang)
    except ExtractionRejected:
        return []
    return [m for m in found if not inside(skip, m.start, m.end)]


def gather(text: str, labels: Iterable[str], extracted_by: str,
           confidence: Optional[float] = None,
           lang: str = DEFAULT_LANGUAGE) -> tuple[list[Mention], list[str]]:
    """Gate a batch. Returns what survived and what was rejected.

    Rejections are returned rather than raised: one bad label from a model
    shouldn't discard the good ones, but it should be visible — the worker logs
    them, because a rising rejection rate is how you find out an extractor has
    started inventing.
    """
    kept: list[Mention] = []
    rejected: list[str] = []
    for label in labels:
        try:
            kept.extend(gate(text, label, extracted_by, confidence, lang))
        except ExtractionRejected:
            rejected.append(label)
    return kept, rejected


@lru_cache(maxsize=4)
def _lemmatizer(lang: str):
    """simplemma if it's installed, else nothing. Cached: loading a language's
    data is slow and every word asks for it."""
    try:
        import simplemma  # type: ignore
    except ImportError:
        return None
    return simplemma


@lru_cache(maxsize=4)
def _stemmer(lang: str):
    try:
        import snowballstemmer  # type: ignore
    except ImportError:
        return None
    return snowballstemmer.stemmer({"de": "german", "en": "english"}[lang])


@lru_cache(maxsize=100_000)
def normalise_word(word: str, lang: str) -> str:
    """One word reduced to the form that decides which topic it belongs to.

    Lemmatise, then stem. Neither alone is enough for German:

    - Stemming misses "Herr"/"Herrn" — Snowball protects short stems, and
      hand-stripping the -n wrongly merges "Stein" with "Stei".
    - Lemmatising misses "Glaube"/"Glauben", because "Glauben" is itself a
      dictionary word.

    Composed, both pairs land on one key while "Herr" and "Herz" stay apart.
    Without either library installed this degrades to casefolding, which merges
    nothing — visible in the topic list rather than silently wrong.
    """
    lemma = word
    simplemma = _lemmatizer(lang)
    if simplemma is not None:
        try:
            lemma = simplemma.lemmatize(word, lang=lang)
        except (ValueError, KeyError):
            lemma = word
    stemmer = _stemmer(lang)
    if stemmer is not None:
        return stemmer.stemWord(lemma.casefold())
    return lemma.casefold()


def key_of(label: str, lang: str = DEFAULT_LANGUAGE) -> str:
    """Lookup key for a label: every word normalised, joined by spaces.

    This is what makes "Herr" and "Herrn" one topic rather than two. It is
    still not a synonym test — "Gnade" and "unverdiente Güte" are different
    keys, and collapsing them needs embeddings and a person to confirm.
    """
    words = [w for w, _, _ in words_with_positions(unicodedata.normalize("NFC", label))]
    return " ".join(normalise_word(w, lang) for w in words)


def is_stopword(word: str, lang: str) -> bool:
    """Function words, checked by lemma so inflections are covered.

    "seinen" lemmatises to "sein", so one entry catches the whole paradigm —
    which is why the lists hold base forms rather than every ending.
    """
    folded = word.casefold()
    if folded in STOPWORDS[lang]:
        return True
    simplemma = _lemmatizer(lang)
    if simplemma is None:
        return False
    try:
        return simplemma.lemmatize(word, lang=lang).casefold() in STOPWORDS[lang]
    except (ValueError, KeyError):
        return False


def detect_language(text: str) -> str:
    """Which language's rules to read this note under.

    By function words, which is what separates languages most cheaply and is
    exactly what the extractor needs to get right: German inflection and German
    stopwords are useless applied to an English note, and vice versa.
    """
    words = [w.casefold() for w, _, _ in words_with_positions(text)]
    if not words:
        return DEFAULT_LANGUAGE
    hits = {
        lang: sum(1 for w in words if w in STOPWORDS[lang]) for lang in LANGUAGES
    }
    best = max(hits, key=lambda lang: hits[lang])
    return best if hits[best] else DEFAULT_LANGUAGE


# ---------------------------------------------------------------------------
# Deterministic extraction — always runs, needs nothing installed
# ---------------------------------------------------------------------------

def words_with_positions(text: str) -> list[tuple[str, int, int]]:
    return [(m.group(0), m.start(), m.end()) for m in WORD.finditer(text)]


def sentence_starts(text: str) -> set[int]:
    """Offsets where a sentence's first word begins, so a capital there proves
    nothing.

    The end of the match, not its start: the match covers the punctuation and
    the space after it, and what matters is where the next word actually
    starts. Getting this wrong makes every capitalised word look like a name.
    """
    return {m.end() for m in re.finditer(r"(?:^|[.!?:;\n]\s*)", text)}


@dataclass
class Candidate:
    """A term the note might be about, and the evidence for it."""

    key: str
    surfaces: list[str]
    capitalised: int
    earned_capital: int
    words: int

    @property
    def count(self) -> int:
        return len(self.surfaces)

    @property
    def label(self) -> str:
        """The form the note writes most often, so the topic reads naturally.

        Always one of the surfaces actually written — never the lemma, which
        may be a form that appears nowhere in the note.
        """
        return max(
            sorted(set(self.surfaces)),
            key=lambda surface: (self.surfaces.count(surface), -len(surface)),
        )


def inside(spans: Optional[list[tuple[int, int]]], start: int, end: int) -> bool:
    """Does this word overlap a span that belongs to something else?"""
    return any(start < s_end and end > s_start for s_start, s_end in spans or [])


def collect_candidates(text: str, lang: str,
                       skip: Optional[list[tuple[int, int]]] = None
                       ) -> dict[str, Candidate]:
    """Every content word and capitalised phrase, grouped by normalised key.

    Grouping by key is what makes counting mean anything in German: "Herr",
    "Herrn" and "des Herrn" are one term appearing three times, not three terms
    appearing once.
    """
    words = words_with_positions(text)
    starts = sentence_starts(text)
    found: dict[str, Candidate] = {}

    def add(surface: str, capitalised: bool, earned: bool, word_count: int) -> None:
        key = key_of(surface, lang)
        if not key:
            return
        entry = found.get(key)
        if entry is None:
            found[key] = Candidate(key, [surface], int(capitalised), int(earned),
                                   word_count)
        else:
            entry.surfaces.append(surface)
            entry.capitalised += int(capitalised)
            entry.earned_capital += int(earned)

    run: list[tuple[str, int]] = []

    def flush_run() -> None:
        nonlocal run
        if len(run) > 1:
            add(" ".join(w for w, _ in run), True, run[0][1] not in starts, len(run))
        run = []

    for surface, start, end in words:
        capitalised = surface[:1].isupper()
        content = len(surface) >= 3 and not is_stopword(surface, lang)
        # A phrase cannot span a full stop. Without this, a sentence ending in
        # a noun and the next one opening with a name become one "phrase".
        if start in starts:
            flush_run()
        # A scripture reference is a citation, not a subject: "Joh" in
        # "Vergleiche Joh 3,16" is the gospel being pointed at, and reading it
        # as a topic makes a concept out of an abbreviation.
        if inside(skip, start, end):
            flush_run()
            continue
        if content:
            add(surface, capitalised, capitalised and start not in starts, 1)
        # A run of capitalised content words is a phrase: "Heiliger Geist",
        # "Neuer Bund". More specific than either word alone, so it stands as
        # its own candidate rather than replacing them.
        if capitalised and content:
            run.append((surface, start))
            if len(run) > MAX_WORDS:
                run.pop(0)
        else:
            flush_run()
    flush_run()

    return found


def is_topic(candidate: Candidate, lang: str) -> bool:
    """Is this term what the note is *about*, rather than something it says?

    The rule differs by language because capitalisation means different things.
    In English a capital mid-sentence is a name, and a name mentioned once is
    still a topic. **In German every noun is capitalised**, so a capital says
    only "this is a noun" — which is true of most of the sentence. There,
    recurrence is the signal that separates a subject from a passing word.
    """
    if candidate.words > 1:
        # A multi-word capitalised phrase is specific enough to stand alone.
        return True
    if candidate.earned_capital:
        # It has been capitalised where grammar didn't require it, so it is a
        # name (English) or a noun (German).
        return True if lang == "en" else candidate.count >= 2
    # Only ever capitalised at the start of a sentence, or never capitalised:
    # the capital proves nothing, so recurrence is all that is left. This is
    # what keeps "Later the law came. Later still, grace." from making a topic
    # out of an adverb.
    return candidate.count >= REPEAT_THRESHOLD


def deterministic_labels(text: str, lang: Optional[str] = None,
                         skip: Optional[list[tuple[int, int]]] = None) -> list[str]:
    """The baseline, over a whole note.

    Note-level on purpose: recurrence is the signal, and a note that returns to
    "Bund" in three separate paragraphs is more about it than one that says it
    three times in a row. Counting inside a paragraph would miss exactly the
    notes that are most clearly about something.
    """
    language = lang or detect_language(text)
    candidates = collect_candidates(text, language, skip)
    chosen = [c for c in candidates.values() if is_topic(c, language)]
    # Most-mentioned first, so a truncated list keeps the note's real subjects.
    chosen.sort(key=lambda c: (-c.count, -c.words, c.label.casefold()))
    return [c.label for c in chosen]


def dedupe_labels(labels: Iterable[str],
                  lang: str = DEFAULT_LANGUAGE) -> list[str]:
    """Same term written twice is one label. Order is kept for stable output."""
    seen: set[str] = set()
    unique: list[str] = []
    for label in labels:
        k = key_of(label, lang)
        if k and k not in seen:
            seen.add(k)
            unique.append(label)
    return unique


# ---------------------------------------------------------------------------
# Local LLM extraction — on this machine, when Ollama is running
# ---------------------------------------------------------------------------

OLLAMA_HOST = os.environ.get("ROOTED_OLLAMA_HOST", "http://localhost:11434")
OLLAMA_MODEL = os.environ.get("ROOTED_OLLAMA_MODEL", "llama3.2")

OLLAMA_PROMPT = """\
Below is a personal Bible-study note. List the topics it is about.

Rules:
- Copy each topic exactly as it appears in the note, character for character.
- Do not tidy, translate, pluralise, singularise, expand an abbreviation, or
  turn a phrase into a term. If the note says "the grace of God", that is the
  topic; do not write "God's grace".
- A topic is a word or short phrase, at most four words.
- Only topics the note is actually about. If there are none, return none.

Return JSON: {"topics": ["...", "..."]}

Note:
"""


def ollama_available(timeout: float = 1.5) -> bool:
    """Is a local model server reachable? Never blocks the pipeline for long."""
    import urllib.error
    import urllib.request

    try:
        with urllib.request.urlopen(f"{OLLAMA_HOST}/api/tags", timeout=timeout):
            return True
    except (urllib.error.URLError, OSError):
        return False


def ollama_labels(text: str, model: Optional[str] = None,
                  timeout: float = 120.0) -> tuple[list[str], list[str]]:
    """Ask a model on this machine what the note is about.

    Returns the labels that survived the gate against this note, and the ones
    that didn't. A model that paraphrases simply contributes nothing, which is
    the correct failure: the deterministic pass already ran and its findings
    stand on their own.

    The surviving labels are the *text's* spelling, not the model's — they come
    back sliced out of the note, so a case difference can't fork a concept.
    """
    import urllib.error
    import urllib.request

    name = model or OLLAMA_MODEL
    payload = json.dumps({
        "model": name,
        "prompt": OLLAMA_PROMPT + text,
        "stream": False,
        "format": "json",
        # Deterministic-ish: the same note should not yield a different graph
        # each time it is re-read.
        "options": {"temperature": 0},
    }).encode("utf-8")

    request = urllib.request.Request(
        f"{OLLAMA_HOST}/api/generate",
        data=payload,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            body = json.loads(response.read().decode("utf-8"))
    except (urllib.error.URLError, OSError, json.JSONDecodeError) as exc:
        raise ExtractionRejected(f"the local model could not be reached: {exc}") from exc

    lang = detect_language(text)
    kept, rejected = gather(text, parse_ollama_topics(body.get("response", "")),
                            f"ollama/{name}", lang=lang)
    return dedupe_labels((m.surface for m in kept), lang), rejected


def parse_ollama_topics(response: str) -> list[str]:
    """Pull the topic list out of a model's answer, forgivingly.

    Shape problems are the model's business and cost nothing to tolerate — a
    missing key or a stray wrapper object just means no topics this time.
    Content problems are the gate's business, and it is not forgiving at all.
    """
    try:
        payload = json.loads(response)
    except json.JSONDecodeError:
        return []
    if isinstance(payload, list):
        topics = payload
    elif isinstance(payload, dict):
        topics = payload.get("topics") or payload.get("concepts") or []
    else:
        return []
    return [t for t in topics if isinstance(t, str) and t.strip()]


# ---------------------------------------------------------------------------
# Chunking
# ---------------------------------------------------------------------------

def chunk_text(text: str) -> list[tuple[int, int, str]]:
    """Split a note into paragraphs, keeping each one's offsets in the note.

    Paragraphs, not sentences: a citation has to carry enough context to mean
    something when read on a topic page, away from the note it came from.
    """
    chunks: list[tuple[int, int, str]] = []
    for match in re.finditer(r"[^\n]+(?:\n(?!\s*\n)[^\n]+)*", text):
        body = match.group(0)
        stripped = body.strip()
        if not stripped:
            continue
        start = match.start() + (len(body) - len(body.lstrip()))
        chunks.append((start, start + len(stripped), stripped))
    return chunks
