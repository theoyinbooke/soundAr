from __future__ import annotations

import unittest

import numpy as np

from core.vocal_events import (
    event_from_cue,
    normalize_cues,
    render_for_vocabulary,
)


class VocalEventTests(unittest.TestCase):
    def test_every_spelling_of_a_laugh_becomes_one_token(self) -> None:
        for cue in ["(laughs)", "[Laughter]", "*laughing*", "( LOL )", "[laughs out loud]"]:
            parsed = normalize_cues(f"{cue} Good evening.")
            self.assertEqual(parsed.canonical, "(laugh) Good evening.", cue)
            self.assertEqual(parsed.events, ["laugh"])

    def test_a_reaction_has_events_and_no_words(self) -> None:
        parsed = normalize_cues("[Laughter] [Applause]")
        self.assertEqual(parsed.canonical, "(laugh) (applause)")
        self.assertTrue(parsed.is_reaction)

    def test_a_bracketed_note_is_never_spoken(self) -> None:
        parsed = normalize_cues("[SFX: door slams] Who is there? *beat* Hello?")
        self.assertEqual(parsed.canonical, "Who is there? Hello?")
        self.assertEqual(parsed.notes, ["SFX: door slams", "beat"])

    def test_parenthesised_prose_stays_prose(self) -> None:
        parsed = normalize_cues("I said (and I mean it) never again.")
        self.assertEqual(parsed.canonical, "I said (and I mean it) never again.")
        self.assertEqual(parsed.events, [])

    def test_rendering_writes_the_engine_vocabulary_or_drops_the_cue(self) -> None:
        canonical = "(laugh) Same, fridge. (sigh) We're all doing our best."
        self.assertEqual(render_for_vocabulary(canonical, "parenthesis").text, canonical)
        self.assertEqual(
            render_for_vocabulary(canonical, "bracket").text,
            "[laugh] Same, fridge. [sigh] We're all doing our best.",
        )
        stripped = render_for_vocabulary(canonical, "none")
        self.assertEqual(stripped.text, "Same, fridge. We're all doing our best.")
        self.assertEqual(stripped.dropped, ["laugh", "sigh"])

    def test_a_reaction_on_a_voice_without_events_is_empty(self) -> None:
        self.assertEqual(render_for_vocabulary("(laugh) (laugh)", "none").text, "")

    def test_unknown_cues_are_not_events(self) -> None:
        self.assertIsNone(event_from_cue("angrily"))
        self.assertEqual(event_from_cue("Clears Throat"), "clears throat")

    def test_ensemble_mix_layers_takes_with_offsets_and_never_clips(self) -> None:
        from bridge import mix_ensemble_takes

        take = np.ones(1_000, dtype=np.float32)
        mixed = mix_ensemble_takes([take, take, take], sample_rate=1_000)
        self.assertEqual(mixed.size, 1_000 + 2 * 90)
        self.assertLessEqual(float(np.max(np.abs(mixed))), 0.9701)
        self.assertGreater(float(mixed[500]), float(mixed[10]))


if __name__ == "__main__":
    unittest.main()
