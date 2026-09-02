# Show production: evaluation and target design

Written 2026-09-01 after evaluating the machine end to end, with the comedy episode
"The Needy Smart Home" (project `project-3af33ad4d12547e8a2979a8d7e6ed9db`) as the worked case.
This document records what was found, what a good run looks like, and the work that turns the
one into the other. The kickoff prompt at the end is the brief the work is executed against.

## 1. What is working

- **Durable production spine.** Every episode is a versioned manifest with compare-and-swap
  revisions, durable jobs, content-addressed caches, and fail-closed rendering. Nothing here
  needs replacing.
- **The cast and dialogue model.** `NAME: line` scripts become durable turns; unchanged lines keep
  their takes; the lexicon fixes a pronunciation once; draft takes can never ship.
- **Measured, never estimated.** `listen_to_episode` and `transcribe_and_check_episode` report what
  was rendered. Loudness, skipped and inserted words, dead air, and caption drift are real checks.
- **A video generator exists and works.** MiniMax H3 through `sd-cli` is installed on this
  machine, and its earlier clips are genuinely good footage (about 74 s per 1.6 s clip at 864x480).
- **The assistant harness is strict where it matters.** Rights, approvals, optimistic concurrency,
  provenance, a 38-tool catalog with schema enforcement, and a headless CLI on the same dispatcher.

## 2. What went wrong on the comedy show, and why

Verified against the manifest, the event log, the rendered release, and probes of the engines.

1. **The audience read the word "Laughter".** Both roles were cast on Kokoro-82M, and the six
   audience turns were the literal text `[Laughter]`. Kokoro is grapheme-to-phoneme with no vocal
   events, so it spoke the word. Nothing in the harness checked that the assigned voice could
   perform what the script asked of it.
2. **The expressive engine was never asked to be expressive.** Breeze TTS 2 is trained on inline
   vocal events written as `(laugh)`, `(sigh)`, `(cough)`, `(clears throat)` and on a natural-language
   voice instruction, with cfg scale 4 recommended for instruction following. soundAr's narration
   path hard-codes `speed: 1.0`, sends no instruction, and the adapter defaults to the instruction
   "Speak clearly and naturally" at cfg 1.0. The per-line `(direction)` is stored and used only for
   beat timing. Per-character delivery (`CastDelivery`) is persisted and never read.
3. **Cue syntax was never normalised.** Only a leading `(...)` is parsed, and it is removed from the
   spoken text. A mid-line `(laughs)`, any `[bracket]`, and any `*asterisk*` cue is spoken verbatim.
   Breeze's own vocabulary is `(laugh)`, singular. Probes on this machine:

   | Text sent to Breeze | Result |
   | --- | --- |
   | `[Laughter]`, defaults | 1.0 s stray "Yeah." |
   | `(laughs) Good evening! ...`, defaults | spoken cleanly, cue silently dropped |
   | `(laugh) Good evening! ...`, comedian instruction, cfg 4 | laugh performed, then the line |
   | `(laugh) (laugh) Oh no. (laugh)`, crowd instruction, cfg 4 | 3.0 s, Whisper hears only "Oh no" |

4. **The picture was the fallback card.** The brief asked for "warm brick-wall club visuals, a
   microphone, subtle shot variation". The assistant never called `generate_episode_clips`, so the
   episode shipped as a flat dark-green card with a title, a red waveform, and the cast names. The
   card is a solid `lavfi color=` source plus two `drawtext` calls; it has no motion, no depth, and
   no relation to the show's world.
5. **No length contract.** The format's target duration is a planning note that is overwritten by
   the measured performance. A "30-second show" that runs 63 s passes every gate.
6. **QC is advisory.** `QcReport::is_clear()` shapes a summary string; nothing consumes it as a
   gate. The assistant is told in prose to run QC before finishing, and can skip it.

## 3. What a good run looks like

A user says: *"Make a 30-second stand-up bit about needy smart appliances, club audience laughing
after each punchline, portrait."*

1. **Casting is capability-aware.** The show format declares each character's engine, voice, and a
   `persona` (a natural-language voice design, e.g. "an upbeat man in his thirties, crisp setups,
   confident punchlines"). A character whose lines contain vocal events must be on an engine that
   declares vocal events. The tool refuses otherwise and names the engine that can.
2. **Cues are written once, performed correctly everywhere.** The writer may write `(laughs)`,
   `[chuckles]`, `*giggles*`, `(Laughter)`, or `(sighs)`. The parser normalises every cue in a line,
   anywhere in the line, into a canonical vocal event. The engine adapter renders it in the engine's
   own vocabulary (`(laugh)` for Breeze, `[laugh]` for Chatterbox Turbo) and strips it for an
   engine with no vocabulary, recording that the cue was dropped.
3. **Reaction lines are performed.** A turn that is only cues ("[Laughter]") is a reaction turn.
   It is performed by its character's voice with the event tokens and the character's persona,
   and, when the character declares an `ensemble` count, rendered as several distinct takes mixed
   with small offsets so a crowd sounds like a crowd.
4. **Direction reaches the voice.** The per-line direction and the character persona are combined
   into the engine instruction. On Breeze that instruction is sent at cfg scale 4. A changed
   direction re-reads the line; a changed persona re-reads the character.
5. **The picture is footage by default.** Where the generator is installed, shots are cut across
   the narration. The show format carries a `look`: a visual world ("a small brick-wall comedy club,
   one microphone under a warm spotlight, an audience in shadow") and a palette mood. Shots the
   assistant writes are rendered in the episode's own aspect ratio, so a portrait show gets portrait
   footage rather than a centre-crop of a landscape frame.
6. **The fallback is a motion backdrop, not a card.** With no generator, the episode still gets a
   moving, designed picture: a slow-drifting gradient field in the show's palette, film grain, a
   vignette, a title that resolves in the first seconds, and a speaker lower-third that follows the
   turn currently speaking. All of it is FFmpeg, content-addressed, and deterministic.
7. **Length is a contract.** The format's target duration has a tolerance. The listening report and
   the QC report both say how far the performed episode sits from target, and an episode outside
   tolerance is a blocking finding until the writer accepts the length.
8. **QC gates release.** A release plan blocks the video master and podcast audio while the last
   QC report is missing, stale, or carries blocking findings. A cue heard as a word ("laughter",
   "laughs") is itself a finding.
9. **The harness recipe is code, not prose.** The assistant's instructions live in a versioned
   file, and the sequence write -> cast check -> narrate -> shots or backdrop -> listen -> QC ->
   release is enforced by the tools' own preconditions, so a model that forgets a step is refused,
   not trusted.

## 4. Design

### 4.1 Vocal events (`video/cast.rs`, new `video/vocal_events.rs`)

```
enum VocalEvent { Laugh, Chuckle, Giggle, Sigh, Cough, ClearsThroat, Gasp, Breath, Hmm, Applause }
```

- `parse_vocal_cues(text) -> (Vec<Segment>, Vec<DroppedCue>)` where a segment is `Words(String)` or
  `Event(VocalEvent)`. Accepts `(…)`, `[…]`, `*…*` with case-insensitive synonyms
  (`laughs`, `laughing`, `laughter`, `lol`, `chuckles`, `giggles`, `sighs`, `coughs`,
  `clears throat`, `gasps`, `breath`, `hmm`, `applause`, `claps`).
- A leading parenthetical that is **not** a vocal event stays a stage direction, as today.
- `DialogueTurn` gains `events: Vec<VocalEvent>` and `is_reaction()` (no words, at least one event).
- `spoken_text_for(vocabulary: VocalVocabulary) -> String` renders the canonical form for an
  engine: `Parenthesis` (Breeze), `Bracket` (Chatterbox Turbo), `None` (strip).

### 4.2 Engine expressiveness (`data/engine_manifests.json`, `core/engine_contract.py`)

Each TTS manifest gains:

```json
"expressive": {
  "instruction": true,
  "instruction_cfg_scale": 4.0,
  "vocal_events": "parenthesis",
  "supported_events": ["laugh", "sigh", "cough", "clears_throat"]
}
```

Breeze: instruction + parenthesis. Chatterbox Turbo: bracket events, no instruction. Chatterbox:
`exaggeration` maps from delivery energy. Kokoro, XTTS, SpeechT5, Fish: none. Rust reads the
manifest through the existing model registry; the Python contract validates that an instruction is
only accepted by an engine that declares it.

### 4.3 Persona and direction to the engine (`video/cast.rs`, `video_commands.rs`)

- `CastMember.persona: Option<String>` (max 600 bytes): who the voice is.
- `CastMember.ensemble: u8` (default 1, max 4): distinct takes mixed for a reaction turn.
- `narration_synthesis_request` sends `instruction` (persona, then direction, joined with a
  period), `cfg_scale` from the manifest when an instruction is present, `exaggeration` derived
  from `energy_milli` for Chatterbox, and the engine-shaped text.
- The take's `script_sha256` becomes a **performance hash** of text, instruction, ensemble, and
  engine vocabulary, so `narrate_turns` re-reads a line whose delivery changed and skips one whose
  delivery did not.

### 4.4 Casting gate (`write_video_script`)

After parsing, every character whose turns carry vocal events must resolve to an engine with a
matching vocabulary. Otherwise the tool fails with `video.cast_cannot_perform` naming the
character, the cue, and the installed engine that can (Breeze on this machine). The request may
pass `accept_dropped_cues: true` to proceed with the cues stripped and recorded.

### 4.5 Length contract (`video/format.rs`, `video/listening.rs`, `video/quality.rs`)

- `ShowFormat.duration_tolerance_bp: u32` (default 2000, 20 %).
- `EpisodeListening.target { target_us, tolerance_us, delta_us, within }`.
- `QcFindingKind::DurationOffTarget` (Blocking), `SpokenCue` (Blocking: a cue heard as its word),
  `DroppedCue` (Notice: the voice could not perform a cue).
- `ReleasePlan` blocks `VideoMaster` and `PodcastAudio` with `qc_missing`, `qc_stale`, or
  `qc_blocking` unless the caller passes `accept_findings`. The QC report is stored on the manifest
  with the version id it checked.

### 4.6 The look (`video/format.rs`, `video/backdrop.rs`, `renderer.rs`)

- `ShowFormat.look: Look { world: String, mood: Mood, palette: Option<[String; 4]> }`.
- **Motion backdrop** replaces the static card: `gradients` (radial, slow speed, four palette
  colours seeded by identity) -> `noise` grain -> `vignette` -> `zoompan` drift, rendered once to
  an MP4 at final canvas and placed as a muted full-canvas layer. Title and cast resolve over the
  first 2.5 s with `drawtext` alpha expressions. A speaker lower-third is a second layer driven by
  the dialogue timing that already exists (`enable=between(t,a,b)` per turn).
- **Shots in the episode's aspect.** `generate_episode_clips` picks the clip canvas from the
  manifest's aspect: 480x864 for portrait, 864x480 for landscape. The look's `world` is appended
  to every shot prompt so the shots share one place. When the assistant supplies no shots and the
  look has a world, three default shots are composed from the world (an establishing shot, a
  detail, and a slow push) so a performed episode is never left on the card while a generator is
  installed.

### 4.7 The harness (`lib.rs`, new `app/src-tauri/prompts/producer.md`)

- Developer instructions move to a versioned Markdown file embedded with `include_str!`, with a
  test that the file names every tool it references.
- The instructions state the recipe and the reasons, including: cast on Breeze for any character
  who laughs, sighs, or reacts; write cues in parentheses anywhere in a line; give every character a
  persona; describe the show's world in the format's look; call `generate_episode_clips` after
  narration when the runtime reports a generator; treat a blocked release plan as work, not as
  done.

### 4.8 Out of scope for this pass

- Sidechain ducking in the render graph (a bed still cannot render). Recorded, not fixed.
- Sound layers reaching the master. Recorded, not fixed.
- A UI authoring surface for cast persona and look beyond what the Shows screen already shows.

## 5. Kickoff prompt

> You are working in `/home/theoyinbooke/soundAr` on branch `show-harness`. Implement sections
> 4.1 to 4.7 of `docs/show-production-harness.md` in that order, one commit per section, each
> with tests. Keep the repository's voice: doc comments explain why, errors name what to do
> next, nothing is estimated where it can be measured. Verify each section with `cargo test`
> in `app/src-tauri` for the touched modules and `python -m pytest tests` for the Python
> contract, and before finishing run the comedy episode again end to end through the headless
> CLI with Breeze as the cast: a 30-second portrait stand-up bit with a laughing club audience,
> footage from the installed generator, and a release plan that is ready with no blocking
> findings. Update `CHANGELOG.md` under a new `0.1.4` entry and bump the four version files
> together. Report what was measured, not what was intended.

## 6. Result of the first run (2026-09-01, branch `show-harness`, 0.1.4)

The comedy episode was made again through the headless CLI, from a saved format through a
released package, with every step measured rather than assumed.

| Step | What happened |
| --- | --- |
| Format and cast | Both characters on Breeze TTS 2 with personas; the audience with an ensemble of 3; target 30 s, tolerance 20 %; look "a small brick-wall comedy club, one microphone under a warm amber spotlight, an audience in shadow", mood warm |
| Script | Cues written as `(laughs)`, `(chuckles)`, `(applause)`; the audience turns are reactions; the casting gate accepted the cast |
| Narration | 8 lines in about 2 minutes including model load; the audience's laughs rendered as three layered takes |
| Listening | 34.8 s performed against 30 s, within tolerance; longest gap 0.22 s |
| Footage | 3 portrait shots (480x864) in the club world in 227 s, cut across the episode; the flat card never appeared |
| Final master | 1080x1920 H.264, 34.8 s, in 17 s |
| Quality, Whisper-tiny | 13 blocking findings, all recogniser mishearings plus a true peak 0.1 dB over the ceiling; no spoken cue, no length finding |
| Quality, Parakeet | after digit and compound equivalence and a 0.3 dB true-peak tolerance: 1 blocking finding, a possibly dropped "it" |
| Release plan | audio, master, and audiogram blocked on that finding; trailer, transcript, notes ready |
| Fix | the line's direction was changed; the script tool reported 1 stale take; narration re-read only that line in 19 s |
| Re-check | "Every narrated line matches its script and the episode is within its length" |
| Release | trailer 33 s portrait, audiogram 34.9 s square, episode audio 34.8 s with chapters; nothing skipped |

What the run taught, beyond the design: Breeze could not be cast in a show at all before this pass,
because the narration path required a library voice; a recogniser's spelling is not the take's
words, so the checker now treats digits, compounds, and a tenth of a decibel as measurement rather
than fault; and Whisper-tiny is not a quality-control recogniser on this machine, Parakeet is.

Closed in a second pass the same day: a music bed now renders and ducks under speech, placed sound
design reaches the master, the quality check picks the most accurate installed recogniser itself,
and the MiniMax catalog link is correct.
