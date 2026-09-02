"""Vocal events: the laughs, sighs, and breaths a script asks a voice to perform.

This is the Python half of ``app/src-tauri/src/video/vocal_events.rs`` and keeps the same
canonical form: an event is written ``(laugh)`` inline, lower case, singular. The desktop
process canonicalises a script when it is written; this module renders the canonical form into
whatever an engine was trained on at the moment a take is requested, and strips it for an engine
that performs nothing rather than letting the engine read "laughter" aloud.

The bridge applies it to every synthesis request, so a direct caller of the local API or the
Generate screen gets the same treatment as a performed episode.
"""
from __future__ import annotations

import re
from dataclasses import dataclass, field

CANONICAL_EVENTS: tuple[str, ...] = (
    "laugh",
    "chuckle",
    "giggle",
    "sigh",
    "cough",
    "clears throat",
    "gasp",
    "breath",
    "hmm",
    "applause",
)

_SYNONYMS: dict[str, str] = {
    "laugh": "laugh", "laughs": "laugh", "laughing": "laugh", "laughter": "laugh",
    "laughed": "laugh", "lol": "laugh", "haha": "laugh", "hahaha": "laugh", "ha ha": "laugh",
    "ha ha ha": "laugh", "big laugh": "laugh", "bursts out laughing": "laugh",
    "laughs out loud": "laugh",
    "chuckle": "chuckle", "chuckles": "chuckle", "chuckling": "chuckle", "soft laugh": "chuckle",
    "small laugh": "chuckle", "quiet laugh": "chuckle", "snicker": "chuckle",
    "snickers": "chuckle",
    "giggle": "giggle", "giggles": "giggle", "giggling": "giggle", "titter": "giggle",
    "titters": "giggle",
    "sigh": "sigh", "sighs": "sigh", "sighing": "sigh", "exhales": "sigh", "exhale": "sigh",
    "heavy sigh": "sigh",
    "cough": "cough", "coughs": "cough", "coughing": "cough",
    "clears throat": "clears throat", "clear throat": "clears throat",
    "clearing throat": "clears throat", "ahem": "clears throat",
    "gasp": "gasp", "gasps": "gasp", "gasping": "gasp", "sharp intake of breath": "gasp",
    "breath": "breath", "breathes": "breath", "breathing": "breath", "inhale": "breath",
    "inhales": "breath", "deep breath": "breath", "takes a breath": "breath",
    "takes a deep breath": "breath",
    "hmm": "hmm", "hm": "hmm", "hmmm": "hmm", "mm": "hmm", "mmm": "hmm",
    "applause": "applause", "applauds": "applause", "clap": "applause", "claps": "applause",
    "clapping": "applause", "cheers": "applause", "cheering": "applause",
    "cheers and applause": "applause", "laughter and applause": "applause",
    "applause and laughter": "applause",
}

VOCABULARIES: tuple[str, ...] = ("none", "parenthesis", "bracket")


def event_from_cue(cue: str) -> str | None:
    """Recognise a cue however the writer spelled it, or ``None`` when it is not one."""
    key = re.sub(r"[^a-z0-9 ]", "", cue.strip().lower())
    key = " ".join(key.split())
    return _SYNONYMS.get(key)


@dataclass
class CueParse:
    canonical: str
    notes: list[str] = field(default_factory=list)

    @property
    def events(self) -> list[str]:
        return events_of(self.canonical)

    @property
    def words(self) -> str:
        return words_of(self.canonical)

    @property
    def is_reaction(self) -> bool:
        return not self.words and bool(self.events)


_GROUP = re.compile(r"\(([^()]*)\)|\[([^\[\]]*)\]|\*([^*]*)\*")


def normalize_cues(text: str) -> CueParse:
    """Read a writer's line into canonical form.

    Every ``(…)``, ``[…]``, and ``*…*`` group is examined. A group naming a vocal event becomes
    the canonical token where it stood. A bracketed or starred group that does not is a note and
    is never spoken. A parenthesised group that is neither is prose and stays in the words.
    """
    notes: list[str] = []

    def replace(match: re.Match[str]) -> str:
        paren, bracket, star = match.group(1), match.group(2), match.group(3)
        inner = paren if paren is not None else bracket if bracket is not None else star
        if inner is None or not inner.strip():
            return match.group(0)
        if star is not None and len(inner.split()) > 4:
            return match.group(0)
        event = event_from_cue(inner)
        if event is not None:
            return f" ({event}) "
        if paren is not None:
            return match.group(0)
        notes.append(inner.strip())
        return " "

    canonical = _GROUP.sub(replace, text)
    return CueParse(canonical=_collapse(canonical), notes=notes)


_CANONICAL_TOKEN = re.compile(r"\((" + "|".join(re.escape(e) for e in CANONICAL_EVENTS) + r")\)")


def events_of(canonical: str) -> list[str]:
    return [match.group(1) for match in _CANONICAL_TOKEN.finditer(canonical)]


def words_of(canonical: str) -> str:
    return render_for_vocabulary(canonical, "none").text


@dataclass
class RenderedLine:
    text: str
    dropped: list[str] = field(default_factory=list)


def render_for_vocabulary(canonical: str, vocabulary: str) -> RenderedLine:
    """Write a canonical line the way one engine reads it.

    Events the vocabulary can write are written its way; the rest are removed and returned so the
    caller can say which cues this voice will not perform.
    """
    vocabulary = (vocabulary or "none").strip().lower()
    if vocabulary not in VOCABULARIES:
        raise ValueError(f"Unknown vocal vocabulary: {vocabulary}")
    dropped: list[str] = []

    def replace(match: re.Match[str]) -> str:
        event = match.group(1)
        if vocabulary == "parenthesis":
            return f" ({event}) "
        if vocabulary == "bracket":
            return f" [{event}] "
        dropped.append(event)
        return " "

    return RenderedLine(text=_collapse(_CANONICAL_TOKEN.sub(replace, canonical)), dropped=dropped)


def _collapse(text: str) -> str:
    collapsed = " ".join(text.split())
    # No space before closing punctuation: "word ." reads as a typo the writer never made.
    return re.sub(r" ([.,!?;:])", r"\1", collapsed)
