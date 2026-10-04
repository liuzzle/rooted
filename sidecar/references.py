#!/usr/bin/env python3
"""
Finding scripture references written inside a note.

Notes cite. "Joh 3,16", "1. Mose 1,1", "Röm 8,28-30" — the reference is part of
how the note argues, and it should be reachable from the note rather than
retyped into the reader. This module finds those references and says exactly
where they are, so the app can make them clickable without rewriting the note.

Same rule as everywhere else here: it *finds*, it does not interpret. A book it
cannot resolve produces nothing rather than a guess — pointing a citation at
the wrong chapter would be worse than leaving it as plain text, because the
reader would trust it.

German first, because that is what these notes are written in: "Joh 3,16" with
a comma, book names like "1. Mose", and abbreviations that differ from the
English ones ("Hes" is Ezekiel, not Hesiod). English forms are recognised too.
"""
from __future__ import annotations

import re
import unicodedata
from dataclasses import dataclass
from typing import Optional

# OSIS code -> the names and abbreviations people actually write, German first.
# Ordinals appear in the forms Germans type them: "1. Mose", "1 Mose", "1Mo".
BOOK_NAMES: dict[str, list[str]] = {
    "Gen": ["1. Mose", "1 Mose", "1Mo", "1Mos", "Genesis", "Gen"],
    "Exod": ["2. Mose", "2 Mose", "2Mo", "2Mos", "Exodus", "Ex", "Exod"],
    "Lev": ["3. Mose", "3 Mose", "3Mo", "3Mos", "Levitikus", "Leviticus", "Lev"],
    "Num": ["4. Mose", "4 Mose", "4Mo", "4Mos", "Numeri", "Numbers", "Num"],
    "Deut": ["5. Mose", "5 Mose", "5Mo", "5Mos", "Deuteronomium", "Deuteronomy",
             "Dtn", "Deut"],
    "Josh": ["Josua", "Joshua", "Jos", "Josh"],
    "Judg": ["Richter", "Judges", "Ri", "Judg"],
    "Ruth": ["Rut", "Ruth"],
    "1Sam": ["1. Samuel", "1 Samuel", "1Sam", "1Sa"],
    "2Sam": ["2. Samuel", "2 Samuel", "2Sam", "2Sa"],
    "1Kgs": ["1. Könige", "1 Könige", "1Kön", "1Ko", "1 Kings", "1Kgs"],
    "2Kgs": ["2. Könige", "2 Könige", "2Kön", "2Ko", "2 Kings", "2Kgs"],
    "1Chr": ["1. Chronik", "1 Chronik", "1Chr", "1 Chronicles"],
    "2Chr": ["2. Chronik", "2 Chronik", "2Chr", "2 Chronicles"],
    "Ezra": ["Esra", "Ezra", "Esr"],
    "Neh": ["Nehemia", "Nehemiah", "Neh"],
    "Esth": ["Ester", "Esther", "Est", "Esth"],
    "Job": ["Hiob", "Job", "Hi"],
    "Ps": ["Psalmen", "Psalm", "Psalms", "Ps"],
    "Prov": ["Sprüche", "Sprichwörter", "Proverbs", "Spr", "Prov"],
    "Eccl": ["Prediger", "Kohelet", "Ecclesiastes", "Pred", "Eccl"],
    "Song": ["Hoheslied", "Hohelied", "Song of Solomon", "Song", "Hld"],
    "Isa": ["Jesaja", "Isaiah", "Jes", "Isa"],
    "Jer": ["Jeremia", "Jeremiah", "Jer"],
    "Lam": ["Klagelieder", "Lamentations", "Klgl", "Lam"],
    "Ezek": ["Hesekiel", "Ezechiel", "Ezekiel", "Hes", "Ez", "Ezek"],
    "Dan": ["Daniel", "Dan"],
    "Hos": ["Hosea", "Hos"],
    "Joel": ["Joel"],
    "Amos": ["Amos", "Am"],
    "Obad": ["Obadja", "Obadiah", "Obd", "Obad"],
    "Jonah": ["Jona", "Jonah", "Jon"],
    "Mic": ["Micha", "Micah", "Mi", "Mic"],
    "Nah": ["Nahum", "Nah"],
    "Hab": ["Habakuk", "Habakkuk", "Hab"],
    "Zeph": ["Zefanja", "Zephanja", "Zephaniah", "Zef", "Zeph"],
    "Hag": ["Haggai", "Hag"],
    "Zech": ["Sacharja", "Zechariah", "Sach", "Zech"],
    "Mal": ["Maleachi", "Malachi", "Mal"],
    "Matt": ["Matthäus", "Matthew", "Mt", "Matt"],
    "Mark": ["Markus", "Mark", "Mk"],
    "Luke": ["Lukas", "Luke", "Lk"],
    "John": ["Johannes", "John", "Joh", "Jh"],
    "Acts": ["Apostelgeschichte", "Acts", "Apg"],
    "Rom": ["Römer", "Romans", "Röm", "Rom"],
    "1Cor": ["1. Korinther", "1 Korinther", "1Kor", "1 Corinthians", "1Cor",
             "1Kor", "1Co"],
    "2Cor": ["2. Korinther", "2 Korinther", "2Kor", "2 Corinthians", "2Cor",
             "2Co"],
    "Gal": ["Galater", "Galatians", "Gal"],
    "Eph": ["Epheser", "Ephesians", "Eph"],
    "Phil": ["Philipper", "Philippians", "Phil", "Php"],
    "Col": ["Kolosser", "Colossians", "Kol", "Col"],
    "1Thess": ["1. Thessalonicher", "1 Thessalonicher", "1Thess", "1Thes"],
    "2Thess": ["2. Thessalonicher", "2 Thessalonicher", "2Thess", "2Thes"],
    "1Tim": ["1. Timotheus", "1 Timotheus", "1Tim", "1 Timothy"],
    "2Tim": ["2. Timotheus", "2 Timotheus", "2Tim", "2 Timothy"],
    "Titus": ["Titus", "Tit"],
    "Phlm": ["Philemon", "Phlm", "Phm"],
    "Heb": ["Hebräer", "Hebrews", "Hebr", "Heb"],
    "Jas": ["Jakobus", "James", "Jak", "Jas"],
    "1Pet": ["1. Petrus", "1 Petrus", "1Petr", "1Pet", "1Pt"],
    "2Pet": ["2. Petrus", "2 Petrus", "2Petr", "2Pet", "2Pt"],
    "1John": ["1. Johannes", "1 Johannes", "1Joh", "1John"],
    "2John": ["2. Johannes", "2 Johannes", "2Joh", "2John"],
    "3John": ["3. Johannes", "3 Johannes", "3Joh", "3John"],
    "Jude": ["Judas", "Jude", "Jud"],
    "Rev": ["Offenbarung", "Revelation", "Offb", "Off", "Apk", "Rev"],
}


@dataclass(frozen=True)
class Reference:
    """A citation found in a note, and where it was written."""

    start: int
    end: int
    surface: str
    book_osis: str
    chapter: int
    verse: Optional[int]
    verse_end: Optional[int]

    @property
    def verse_id(self) -> Optional[str]:
        """The BCV key this points at, when it names a verse."""
        if self.verse is None:
            return None
        return f"{self.book_osis}.{self.chapter}.{self.verse}"


def _normalise(name: str) -> str:
    """Fold a written book name to something comparable.

    Dots and spaces are noise: "1. Mose", "1 Mose" and "1Mose" are one name.
    Case is folded, but umlauts are *not* — "Römer" and "Romer" both appear in
    practice, so both spellings are registered instead of stripping accents,
    which would also merge names that differ only by an accent.
    """
    return unicodedata.normalize("NFC", name).replace(".", "").replace(" ", "").casefold()


def _variants(name: str) -> set[str]:
    """The ways one book name actually gets typed.

    Two axes, both mechanical, both generated rather than listed by hand — a
    hand-written list of 66 books times every spelling is where the gaps come
    from:

    - **Ordinal spacing.** "1. Korinther", "1 Korinther" and "1Korinther" are
      the same reference; so are "1 Cor", "1. Cor" and "1Cor".
    - **Umlauts.** "Römer" is also typed "Roemer" and "Romer", depending on the
      keyboard and the hurry.
    """
    forms = {name}
    ordinal = re.match(r"^(\d)\s*\.?\s*(.+)$", name)
    if ordinal:
        number, rest = ordinal.groups()
        forms |= {f"{number}. {rest}", f"{number} {rest}", f"{number}{rest}"}

    expanded = set()
    for form in forms:
        expanded.add(form)
        umlaut_free = (
            form.replace("ä", "ae").replace("ö", "oe").replace("ü", "ue")
            .replace("Ä", "Ae").replace("Ö", "Oe").replace("Ü", "Ue")
            .replace("ß", "ss")
        )
        stripped = (
            form.replace("ä", "a").replace("ö", "o").replace("ü", "u")
            .replace("Ä", "A").replace("Ö", "O").replace("Ü", "U")
            .replace("ß", "ss")
        )
        expanded |= {umlaut_free, stripped}
    return expanded


def _all_forms() -> dict[str, str]:
    """Every spelling that resolves, mapped to its OSIS code."""
    forms: dict[str, str] = {}
    for osis, names in BOOK_NAMES.items():
        for name in list(names) + [osis]:
            for variant in _variants(name):
                forms.setdefault(variant, osis)
    return forms


FORMS = _all_forms()
LOOKUP = {_normalise(form): osis for form, osis in FORMS.items()}

# Longest first, so "1. Johannes" wins over "Johannes" and "1. Mose" is never
# read as a stray "1" followed by a book.
_NAMES = sorted(FORMS, key=len, reverse=True)

# "Joh 3,16" · "1. Mose 1,1" · "Röm 8,28-30" · "Ps 23" · "John 3:16"
#
# German separates chapter and verse with a comma, English with a colon; both
# are accepted, along with a dot, which people also type. A trailing range is
# optional, and so is the verse — "Ps 23" is a whole chapter.
REFERENCE = re.compile(
    r"\b(?P<book>" + "|".join(re.escape(n) for n in _NAMES) + r")\.?\s*"
    r"(?P<chapter>\d{1,3})"
    r"(?:\s*[,:.]\s*(?P<verse>\d{1,3})"
    r"(?:\s*[-–]\s*(?P<verse_end>\d{1,3}))?)?",
    re.IGNORECASE,
)


# Chapters in each book whose name starts with an ordinal. Needed to read
# numbered lists: in "2. Johannes 14, 15" the 2 is the list item, not the
# second letter of John — which has one chapter. "1. Mose" is not here: "Mose"
# alone is no book, so there is nothing to confuse it with.
ORDINAL_CHAPTERS = {
    "1Sam": 31, "2Sam": 24, "1Kgs": 22, "2Kgs": 25, "1Chr": 29, "2Chr": 36,
    "1Cor": 16, "2Cor": 13, "1Thess": 5, "2Thess": 3, "1Tim": 6, "2Tim": 4,
    "1Pet": 5, "2Pet": 3, "1John": 5, "2John": 1, "3John": 1,
}

# What may continue a reference: "Hebräer 10,5-10 + 14", "Apg 2,22-32 + 13,35",
# "Röm 4,3; 5,1", "Joh 3,16 und 18". A comma is deliberately *not* a joiner —
# "Joh 3,16, 4" is too ambiguous to guess at.
CONTINUATION = re.compile(
    r"\s*(?:\+|;|\bund\b|\band\b|&)\s*"
    r"(?:(?P<chapter>\d{1,3})\s*[,:]\s*)?"
    r"(?P<verse>\d{1,3})(?![\d.]*\s*\.\s*[A-ZÄÖÜ])"
    r"(?:\s*[-–]\s*(?P<verse_end>\d{1,3}))?"
    r"(?!\s*[.]?\s*[A-Za-zÄÖÜäöü])",
    re.IGNORECASE,
)


def find_references(text: str) -> list[Reference]:
    """Every scripture reference written in `text`.

    Overlaps are impossible by construction — the scan moves forward — and a
    book name that doesn't resolve yields nothing at all. Nothing here checks
    that the chapter or verse *exists*; that is the reader's job, and it needs
    an installed translation to answer.
    """
    found: list[Reference] = []
    for match in REFERENCE.finditer(text):
        osis = LOOKUP.get(_normalise(match.group("book")))
        if osis is None:
            continue
        start = match.start()
        chapter = int(match.group("chapter"))
        if chapter > ORDINAL_CHAPTERS.get(osis, chapter):
            # A chapter this book doesn't have: the ordinal was a list number.
            book = match.group("book")
            bare = re.sub(r"^\d\s*\.?\s*", "", book)
            osis = LOOKUP.get(_normalise(bare))
            if osis is None:
                continue
            start = match.start("book") + len(book) - len(bare)
        verse = match.group("verse")
        verse_end = match.group("verse_end")
        ref = Reference(
            start=start,
            end=match.end(),
            surface=text[start:match.end()],
            book_osis=osis,
            chapter=chapter,
            verse=int(verse) if verse else None,
            verse_end=int(verse_end) if verse_end else None,
        )
        found.append(ref)
        found.extend(_continuations(text, ref))
    return found


def _continuations(text: str, first: Reference) -> list[Reference]:
    """The references that carry on from `first` without repeating the book.

    Each gets its own span — only the numbers written — so it is clickable
    where it was written. A bare number continues the previous reference's
    chapter when that named a verse ("10,5-10 + 14" is verse 14), and is a
    chapter when it didn't ("Ps 23 + 24"). "13,35" names both.

    A number followed by a word is left alone: in "Joh 3,16 und 2 Kinder" the
    2 belongs to the words after it, not to John.
    """
    out: list[Reference] = []
    previous = first
    at = first.end
    while True:
        m = CONTINUATION.match(text, at)
        if not m:
            return out
        number = int(m.group("verse"))
        end_number = int(m.group("verse_end")) if m.group("verse_end") else None
        if m.group("chapter"):
            chapter, verse = int(m.group("chapter")), number
        elif previous.verse is not None:
            chapter, verse = previous.chapter, number
        else:
            chapter, verse, end_number = number, None, None
        start = m.start("chapter") if m.group("chapter") else m.start("verse")
        ref = Reference(
            start=start,
            end=m.end(),
            surface=text[start:m.end()],
            book_osis=first.book_osis,
            chapter=chapter,
            verse=verse,
            verse_end=end_number,
        )
        out.append(ref)
        previous = ref
        at = m.end()
