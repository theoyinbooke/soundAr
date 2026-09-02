# Changelog

## 0.1.6 - 2026-09-02

- Opened a video from Recent in Video Studio. Selecting a video, draft or finished, used to land on
  History with a grid of masters above an unrelated audio record; it now opens that video where
  it can be played, revised, and exported. History is audio generations only.
- Stopped reporting a freshly installed runtime as "setup required". The readiness check compared
  the runtime against a hard-coded Transformers pin; it now reads the pin from the same
  requirements file the installer uses.
- Let the idle chat grow and rise on a full-screen canvas: a wider column, larger type, a taller
  composer, sitting higher. Ordinary window sizes are unchanged.

## 0.1.5 - 2026-09-02

- Made a room laugh like a room. Two identical `(laughs)` lines used to produce the same seed, the
  same instruction, and the same audio twice: a laugh track. A set of reactions is now shaped by
  where each falls in the episode - an opening that is short and warm, a middle that builds, the
  biggest laugh at the close, applause only where the writer asked for it - and every occurrence
  is its own take. A direction the writer puts on a reaction line always wins over the shape.
- Gave a reaction a ceiling. A show's clock now carries the longest a laugh or a round of applause
  may run (3.5 seconds by default); a take past it is cut to length and faded out rather than sped
  up, so a laugh trails off and the next line arrives while the room is still warm.
- Let a re-read line take its own length. A replaced take used to be squeezed into the slot of
  the take before it, so a bigger laugh could not land; a performed line now takes its measured
  length and the lines after it are laid out again on the clock, with the generated shots re-cut
  and the backdrop refitted to the new span.
- Retired a rewritten line's take with the line. Re-applying a script that changed a line's words
  dropped the old take's binding but left its audio on the timeline, where the new take then
  landed on top of it.
- Made a new master supersede the old one. A final render was appended beside the previous
  master and the release took whichever came first, so a re-rendered episode could ship its
  earlier cut and its quality check read as stale. A base final render now retires the masters
  before it, and a release always takes the newest published master.
- Named finished things after the work. A master, an audio episode, a trailer, an audiogram, and
  a publish package are titled and saved as the episode's name - "the-needy-smart-home-trailer.mp4",
  never "final" or a hash - and a render's label is the work's name rather than "Final master".
- Burned the speaker cards. The final render only burned the subtitle layer when the episode had
  captions, so a performed script with none lost its title and its speaker name cards. It now
  burns whenever there is dialogue.
- Tidied Recent. A performed line's take no longer appears in the sidebar as its own item; the
  episode it belongs to is listed there already.
- Showed the performed length against the show's target on the episode screen.
- Raised the idle greeting on a tall full-screen window and removed the History rail entry, which
  duplicated Recent.
- Moved the shared inference foundation and every layered engine to transformers 5.10.1, closing
  the high-severity advisories against 5.5.0. The three standalone runtimes (Breeze, Fish Speech,
  ACE-Step) stay on the versions their upstream code pins; their remaining advisories are recorded
  as accepted risk with the reason, because those runtimes perform no network I/O and load only
  revision-pinned local weights.
- Repaired the release workflow, which had defined `args` twice since 30 August and so could not
  publish any release of the reset version line; the pre-reset releases were retired so 0.1.x is
  the line that updates come from.

## 0.1.4 - 2026-09-01

- Performed vocal cues instead of reading them. A comedy episode wrote its audience as
  `[Laughter]` and cast it on a voice that performs no vocal events, so the audience said the word.
  Every spelling of a cue - `(laughs)`, `[chuckles]`, `*giggles*`, `[Laughter]` - now becomes one
  canonical `(laugh)` token wherever it stands in a line, and is rendered into the engine's own
  vocabulary when a take is requested: Breeze TTS 2 reads `(laugh)`, Chatterbox Turbo reads
  `[laugh]`, and a voice with no vocabulary has the cue removed and the removal recorded rather
  than spoken. A line that is only cues is a reaction, and a character may declare an ensemble so
  a room laughs as several people. `write_video_script` refuses a cast that cannot perform its own
  script and names the voice that can. The Generate screen and the local API get the same
  treatment, because the bridge renders cues for every caller.
- Steered a voice with its character. A cast member now carries a persona - who the voice is, as
  a voice-design instruction - and it reaches an instruction-following engine together with the
  line's direction, at the guidance scale the engine's manifest asks for. Each take records the
  performance it was produced under, so a changed persona or direction re-reads exactly the lines
  it affects and nothing else. Engine manifests declare what each voice can do beyond words.
- Made an episode's length a contract. A show format carries a tolerance around its target, an
  episode inherits both, the listening report says how far the performed length sits from the
  target, and a performed episode outside tolerance is a blocking quality finding. Previously a
  thirty-second show that ran sixty-three seconds passed every gate.
- Taught quality control what a performed cue sounds like. A laugh a recogniser writes down as
  "ha ha ha" is the cue performed and never an inserted word; a take that says "laughter" is
  reported as a spoken cue; a cue the cast voice could not perform is reported as dropped.
- Made the quality check a release gate. Each check is recorded against a fingerprint of
  everything it listened to, and the release plan blocks the audio episode, the master, and the
  audiogram while that record is missing, stale, or blocking. Findings can be accepted explicitly;
  the absence of a check cannot.
- Retired the flat card. An audio-only episode with no generator now gets a motion backdrop drawn
  by FFmpeg alone: a slow-drifting radial field in the show's palette under grain and a vignette,
  the title resolving over the first seconds, and a speaker card naming each character as they
  begin to speak.
- Gave a show a look. A format names the world the audience sees, a mood, and optionally a
  palette. Every generated shot is set in that world, shots are generated in the episode's own
  aspect so a portrait show gets portrait footage, a writer who supplies no shots gets three views
  of the world, and the backdrop is coloured after the mood. The release plan refuses to ship a
  backdrop while a video generator could have made footage, unless the writer chooses it.
- Let a voice-design engine be cast as itself. A character whose voice_id is `__engine_default__`
  on an engine that declares a default voice mode, such as Breeze TTS 2, is performed by the
  engine's own designed voice steered by the character's persona; previously the Video Studio
  narration path required a library voice, so Breeze could not be cast in a show at all.
- Let a music bed render. A bed must duck under the speech it plays under, and until now the
  render refused any timeline that carried a ducking envelope because no filter implemented it.
  The graph now splits the speech track and compresses the bed by it, so a bed dips while a voice
  speaks and returns between lines; a real render proves the drop.
- Let sound design reach the master. Placed room tone, ambience, and one-shots were registered,
  validated, shown, and silently absent from every render. Each placed sound now enters the mix at
  its range with its fades and gain, loops to fill where asked, and ducks under speech where asked.
- Chose the recogniser for quality control. `transcribe_and_check_episode` no longer needs a
  model id: soundAr picks the most accurate installed recogniser (Parakeet first), because a small
  Whisper mishears takes it should pass and fails an episode on its own spelling.
- Corrected the MiniMax H3 catalog entry's source link, which pointed at Kokoro.
- Moved the producer's standing instructions out of a string literal into
  `app/src-tauri/prompts/producer.md`, rewritten as the recipe a show is made by, with a test that
  holds every tool it names to the catalog.

## 0.1.3 - 2026-08-31

- Gave a performed episode moving pictures. Where a local video-generation model is installed,
  soundAr now generates short shots and cuts them across an episode's narration instead of drawing
  a static card. Shots are described by whoever is writing the show - an episode's own words
  describe a conversation, and a shot has to say what is on screen - and a handful are repeated
  across the clock, because a clip costs about a minute of local compute for under two seconds of
  footage. The drawn card remains the automatic fallback, so an episode is still packageable on a
  machine with no generator.
- Found the local video generator the way FFmpeg is found. `sd-cli` is discovered with the same
  discipline as every other media tool, reported in runtime status whether present or absent, and
  overridable with `SOUNDAR_SD_CLI_PATH`.
- Refused an undistilled denoiser rather than running one. soundAr generates at eight steps with
  guidance fixed at 1.0; an undistilled checkpoint given those settings emits black frames and
  exits successfully, so a model directory holding both now resolves to the distilled one and says
  why when it holds neither.
- Recorded the video model in the catalog without bundling it. The weights are open but licensed
  for a limited territory, so `data/curated_models.json` carries the licence, the installation
  note, and the measured performance rather than soundAr shipping or downloading them.

## 0.1.2 - 2026-08-30

- Drew a picture for episodes that have none. A script performed by voices produces sound and
  nothing to look at, and every video deliverable needs a picture, so an audio-only episode could
  not be rendered or packaged at all. soundAr now draws a cover from what the episode already knows
  about itself - its name, its cast, and its canvas - using FFmpeg alone, with no image model and no
  network. The card is derived, so the same episode always produces the same card; it is recorded as
  generated rather than as artwork the user supplied; and an episode that already has a picture
  keeps it. It is drawn automatically once a script has been performed, and can be drawn or redrawn
  from the Shows episode screen, the `ensure_episode_cover` agent tool, and the headless CLI.
- Let a performed conversation back its own scene. A conversation is many takes with beats between
  them, and the render contract demanded a single clip spanning the scene, so no spoken scene could
  ever be rendered. A scene now counts as backed when it begins where its first line begins, ends
  where its last line ends, and contains only lines that have a published take.
- Made an episode as long as it was performed. An episode carried its show format's planning target
  as its actual length, so a seventeen-second conversation rendered as ten minutes of silence held
  under a picture.
- Removed a flaky test that would have failed roughly one run in five. Several media tests write a
  small script and then execute it, and a sibling test forking in that window inherits the still-open
  write descriptor, so the exec fails with `ETXTBSY`. Making an executable in a test now waits until
  it can actually be run. Forty consecutive suite runs pass where the previous arrangement failed
  eight times in seventy.
- Kept a generated scene in step with its performance. The scene soundAr builds for a performed
  script was only ever built once, so an episode narrated again kept whatever length it was first
  given. soundAr now keeps the scene it owns matching the performance while never rewriting
  divisions the author made, and leaves a matching scene untouched rather than bumping the revision
  and invalidating every render that depends on it.
- Made a generated cover's address cover how it is drawn, not only what it is drawn from. Type
  sizes are derived from the canvas, so hashing the inputs alone let a change to the drawing rules
  keep serving cards drawn by the old ones - silently, and for good.
- Fixed generated stills being written in the wrong format. FFmpeg picked the image encoder from the
  output filename, and a card is written to a staging name with no extension, so a file named `.png`
  was actually a JPEG - which then failed to load as a still layer during assembly.

## 0.1.1 - 2026-08-30

The version line restarts here. Everything below this entry shipped under the numbers `0.3.0`
through `0.8.8`; that history is kept exactly as it was rather than renumbered, because those are
the numbers those builds were released under. What changes is the pace from here: patches advance
one at a time within `0.1.x` and the minor moves only when the patch number has run a long way,
not whenever a release feels significant.

Because 0.1.1 sorts below 0.8.8, package managers and the updater treat this as a downgrade rather
than an upgrade. Installing over an existing 0.8.8 needs `dpkg -i --force-downgrade`.

- Gave a project its own screen. Opening an audio production used to reveal a composer beneath the
  table it was chosen from, so the library and the work competed for the same page. A project now
  replaces the library with a dedicated screen and returns to it explicitly, and the library no
  longer opens whichever project happened to sort first.
- Gave an episode its own screen. Cast, script, and release readiness were a panel appended under
  the Shows tables; reading them meant reading past the list of every other episode. An episode is
  now a screen of its own, reached by opening its row and left by going back.

## 0.8.8 - 2026-08-30

- Put every production in one table. Video and audio work were listed on separate halves of the
  Projects page, so finding something meant knowing which kind it was before looking for it. There
  is now a single filterable table of everything made locally, and a row opens the workspace that
  production belongs to - Video Studio for a video, the chapter composer for an audio project.
- Gave Phase 12 a front door. Casts, scripts, pronunciation, score, sound design, and release
  readiness were all reachable by the assistant and by the command line, but nowhere in the app, so
  none of it could be seen without asking for it. A Shows view now lists saved formats and every
  episode, and opening an episode reports its cast, how many lines are performed, drafted, and never
  narrated, and what each release deliverable is still waiting on.

## 0.8.7 - 2026-08-30

- Made a performed script renderable. A script written as dialogue has turns but no scenes, and
  rendering, captions, and chapters are all scene-shaped, so an episode narrated from a script could
  not be previewed or exported at all. Narration now builds one scene spanning the performed
  dialogue - the whole episode until the author divides it - and never replaces divisions the author
  already made.

## 0.8.6 - 2026-08-30

- Reported which character performed a take. The take records it, but the project view left it out,
  so a line's performer was invisible to the UI and to the assistant even though the manifest knew
  it.

## 0.8.5 - 2026-08-30

- Let a script run longer than its source. Placing a performed line past the end of the timeline
  left the imported source's gap-preserving track no longer covering it, so narration failed with
  `video.timeline_gap`. Lengthening an episode now extends each of those tracks with a declared gap
  rather than leaving an implicit hole for the renderer to interpret.

## 0.8.4 - 2026-08-30

- Performed a script for the first time. Narrating a line that had never been read generated the
  speech and then failed, because the only way to attach a take was to replace a clip that already
  existed. A first performance now places its line on a dedicated dialogue track at the take's own
  measured length, positioned after everything already spoken plus that line's beat, so a written
  conversation is laid out in the order and timing the script asks for. Replacing an existing line
  still keeps its slot, so the timeline does not move under it.

## 0.8.3 - 2026-08-30

- Accepted a dialogue line as a narration target. Performing a script generated the speech and then
  refused to attach it, because a take could only be aimed at a binding, a clip, or a scene. A line
  has a take whether or not it has been placed on the timeline yet.

## 0.8.2 - 2026-08-30

- Fixed narrating a character's line. A narration take records both the engine's speaker - which is
  a voice route - and the character who performs the line. Those were being stored in one field, so
  a preset voice and a character name fought over it and performing any line failed with
  `video.voice_speaker_mismatch`. The character is now recorded separately, and a take is checked
  against the voice route its cast entry declares rather than against a name.
- Resolved a line's voice through the installed voice library instead of assuming it, so a preset
  route names its preset voice and a cloned one carries its consent-backed reference.

## 0.8.1 - 2026-08-30

- Made the episode surface findable. The Video Studio inspector now always offers a Cast tab
  instead of hiding it until a cast already existed, and it shows the whole episode in one place:
  every line as performed, standing in with a draft, or not yet read, plus the pronunciation rules,
  music cues, and sound design placements that govern it. An episode started from a show format says
  which format and revision it inherited from.
- Fixed narrating a line. A turn-scoped narration was still being checked against a scene it does
  not have, so performing a script failed outright with `video.scene_not_found`. A line's identity
  is now its own text after its pronunciation rules, and its durable checkpoint is scoped to the
  line rather than to a scene, so two lines in one scene cannot adopt each other's in-flight work.
- Stopped reporting unchanged lines as valid takes. Committing a revised script said "N existing
  takes remain valid" while counting lines that had never been performed; it now reports new,
  unchanged, and dropped counts separately.

## 0.8.0 - 2026-08-30

- Added a cast. A character is bound to one voice, model, language, and delivery, and a
  speaker-attributed script - `NARRATOR: line` - becomes ordered dialogue turns. A turn's identity
  is its character and its words, so re-applying a revised script keeps every unchanged line's
  existing take: reordering costs nothing, and fixing a typo on line 40 of a 200-line script
  re-reads exactly that line. An unknown speaker or an unclosed stage direction is reported against
  its source line rather than narrated by whichever voice happened to be selected.
- Gave dialogue a performance clock. Beats are derived from the script - a reply is faster than the
  same character continuing a thought, a line that trails off or carries a pause direction earns
  silence, and an `(interrupting)` direction overlaps the previous line instead of waiting. A beat
  you set by hand survives every later edit that leaves its own line alone. Retiming reassembles the
  episode without re-reading any line.
- Added a pronunciation lexicon with character, project, and global scopes. Fix an invented name
  once and every take of every character says it correctly. A take records the exact rules that
  produced it, so changing a rule re-reads only the lines that rule governs, and a project's rules
  can never reach another project.
- Added a score cue sheet. A cue's role - sting, bed, transition, or outro - decides where it sits
  and how it is mixed. A bed placed on the timeline is given its ducking envelope automatically,
  sidechained to the track that actually carries narration, because a bed that does not duck is the
  most common way a mix buries its own dialogue. Generated music that runs long is trimmed with a
  musical tail rather than cut off; music that falls well short is reported for regeneration rather
  than padded with silence.
- Added sound design from your own audio. Room tone runs under a whole scene, which is what removes
  the digital silence between takes that makes an episode sound assembled. One-shots anchor to the
  line they punctuate so a later edit cannot move them away from it. Nothing here generates audio:
  placements label media you registered through the ordinary import path.
- Added show formats. A format holds the decisions that do not change between episodes - cast,
  pronunciation, timing, captions, canvas, loudness, usual length, opening and closing music - so a
  new episode starts as a brief. Instantiation copies, so editing a format never changes an episode
  that already shipped.
- Added release export. One episode produces the audio episode carrying its chapter marks, a short
  vertical trailer, and a square audiogram, each registered as checksummed playable media. The
  trailer moment is chosen by running soundAr's existing candidate analyst over the episode's own
  narration, so generated work is reviewed by the rules already proven on imported source.
- Added production quality control. soundAr listens back to every narrated line with a local model
  and compares it to the script that take was asked to speak, so a skipped word or a mispronounced
  name is reported per line rather than discovered by a listener. It measures the master's loudness
  and true peak against the format's target. It reports only: it never rewrites a script, re-renders
  a take, or adjusts a mix. A line nobody listened back to is reported as unchecked, never as
  passed.
- Gave the assistant ears. `listen_to_episode` reports the episode as rendered - which lines were
  performed, how long each runs, how speaking time divides between characters, where the silences
  fall - so a judgment about pacing responds to something measured rather than to the plan the
  assistant wrote. Every number comes from a published take with a measured duration; nothing is
  estimated from word counts.
- Added draft mode. Hear a long episode quickly with the fastest installed voice, then promote only
  the lines worth keeping, which re-reads just those lines. A draft take can never be exported as a
  master or published as a release member, and the new Cast tab shows each line as performed,
  standing in, or not yet read.

## 0.7.0 - 2026-08-29

- Installed updates in place instead of sending you to the browser. The Debian package now downloads, verifies, and installs through `pkexec` and restarts itself, so an update is one click and one permission prompt. Previously only AppImage installs could self-update and the Debian build just opened the release page.
- Dropped the AppImage. It was the only artifact that could self-update before, but nothing depended on it, it cost 2m34s of every build against 32s for the Debian package, and its bundled GStreamer stack was a recurring packaging fault. Everything now ships as one Debian package.
- Cached Rust in CI and release. Neither workflow cached it while npm and pip both were, so every run compiled the whole dependency graph from scratch, twice per release.
- Opened soundAr on a full-screen chat canvas instead of New generation: the greeting and composer sit high on an empty page, and sending hands the whole content column to the thread with the composer docked. Plans, activity, audio artifacts, video results, and approvals all surface inline.
- Added a Chat/Classic toggle to the top bar. One assistant instance serves both layouts and is re-placed by the shell grid, so switching modes never remounts the pane and a live Codex conversation survives the switch; any sidebar destination leaves chat on its own.
- Made Breeze TTS 2 the default voice model wherever speech is created without a named model, including the assistant's speech tools, and taught the expressive route to recognize Breeze's `cfg_scale` control. Fast, Clone, and Multilingual keep their existing engines because Breeze supports neither reference cloning nor preset voices.
- Fixed Linux Video Studio playback with a local media server and resolved media URLs, added caption presets and scene patching, and held opening-frame posters until playback starts across the preview player and master card.
- Raised Fish Speech to torch 2.6.0 for CVE-2025-32434, where `torch.load` honors a pickled payload even with `weights_only=True`, and to hydra-core 1.3.4 for CVE-2026-68508.
- Refused model installs whose configuration carries dynamic kernel fields, scanning nested sub-configs. Breeze, Fish Speech, and ACE-Step remain on their qualified Transformers 4.57.x pins, which resolve and execute remote kernel code named by a checkpoint config (CVE-2026-4372); this gate is the compensating control.
- Held the new dependency floors and the config gate in the runtime dependency security policy so a later requirements edit cannot quietly reintroduce them.
- Ran the Python suite with pytest in CI and release. `unittest discover` cannot collect bare test functions with fixtures and was silently skipping five tests, including the check that only the pinned ACE-Step code sync is trusted.

## 0.6.1 - 2026-08-28

- Rebuilt Video Studio as a compact, resizable composition workspace with a larger centered portrait canvas, title-only scene rail, collapsible five-lane timeline, top-level export actions, responsive layouts, and exact project/history reopening.
- Added eight rendered caption treatments, deterministic source-word paging, per-scene caption selection, and direct canvas drag/resize placement that stays synchronized with the source clock from preview through final export.
- Added durable split, trim, reorder, merge, and visual-layer timeline edits with version checks, idempotent operations, preserved gaps, targeted cache invalidation, and revision-bound undo/redo behavior.
- Added first-class PNG, JPEG, and WebP illustration layers with crop, fit, fades, pan-and-zoom motion, FFmpeg image-sequence rendering, generated-image provenance, and playable animated-podcast masters.
- Added app-closed `soundar-desktop agent` video tools so authenticated Codex workflows can register generated visuals, edit timelines, render, export, and retrieve durable projects through the same services as the desktop UI.
- Bound every visual import to a short-lived, one-use exact-file receipt from the native picker or an authenticated, pinned Codex generation result, including hostile PATH/environment, symlink, hardlink, replacement, replay, and schema-migration defenses.
- Fixed automatic Codex detection when opening or reopening the Assistant and removed the false “CLI not detected” state that previously required a manual rescan.

## 0.6.0 - 2026-08-28

- Added a first-class Linux Video Studio for authorized link/local ingest and prompt/audio starts, with internal timestamped transcription, candidate review, scene planning, a source-clock timeline, fast portrait previews, and local MP4/package export.
- Added versioned manifests, project locks, durable cancellable/resumable media jobs, atomic publication, crash recovery, content-addressed scene caches, and Projects/History/Assistant master surfacing.
- Added shared Video Studio tools to the existing authenticated Codex assistant so conversational research, planning, creation, revision, monitoring, preview, export, and publish-package workflows use the same native services as the UI.
- Added captions, title and speaker cards, animated podcast waveforms, crop/layout control, speech and music mixing, NVENC final rendering with verified software fallback, and targeted invalidation for incremental revisions.
- Hardened link and local-media intake with per-URL rights evidence, single-source defaults, public-only HTTPS proxy confinement, bounded tools and outputs, ordinary-media protocol/demuxer allowlists, private managed storage, disk reservations, checksum validation, and atomic publish packages.
- Added managed yt-dlp/EJS/Node and faster-whisper discovery, CUDA word-timestamp transcription with preserved gaps, exact-machine FFmpeg/NVENC benchmarks, and a three-run qualified Whisper-tiny plus single-NVENC overlap envelope for the RTX 4080 Laptop GPU.
- Added a compact, responsive Video Studio surface and coherent final-master presentation across Video Studio, Projects, History, and the Assistant without reintroducing automatic navigation collapse or decorative UI accents.

## 0.5.5 - 2026-08-27

- Kept Fish Speech in one warm resident worker by default while preserving durable queued requests and the global scheduler capacity for independent engines.
- Added progressive Fish audio previews with validated job-scoped storage, first-audio timing, atomic WAV updates, completion cleanup, and interrupted-session recovery.
- Surfaced active local generation progress in the assistant, with playable progressive previews for single audio and one compact aggregate state for project renders.
- Made the assistant a practical desktop split pane without automatically collapsing navigation, and reflowed dense generation, project, voice, model, compare, and live workspaces inside the remaining width.
- Added repeatable cold/warm Fish benchmark tooling and retained compilation as an explicit experiment because its faster warm inference did not justify its measured first-run latency.

## 0.5.4 - 2026-08-27

- Kept the populated Projects workspace within 320–390 px phone viewports and prevented controls inside a closed Production panel from overlapping mobile navigation.

## 0.5.3 - 2026-08-27

- Recovered legacy assistant-created project masters into soundAr's managed artifact history so existing projects and assembled audio resurface automatically.
- Added an app-owned assistant tool for exporting, registering, attaching, playing, and copying a final project master instead of returning an inaccessible filesystem path.
- Refreshed Projects immediately after assistant changes, preserved assistant-owned project metadata during edits, and exposed the final master in the Production panel.
- Kept project conversations focused on the final assembled master while collapsing completed tool calls into a compact activity summary; single-audio requests still show their playable result.

## 0.5.2 - 2026-08-27

- Reorganized the assistant composer so model and Studio access stay together while reasoning sits beside the Send action.
- Replaced full-width assistant sheets with compact trigger-anchored model, reasoning, and access popovers, including hidden scroll chrome.
- Removed false canvas overflow and nonessential sidebar/assistant scroll tracks while preserving scrolling when content genuinely exceeds the viewport.

## 0.5.1 - 2026-08-27

- Fixed Codex discovery on Linux desktops with multiple installations by selecting the newest valid CLI across PATH, NVM, npm, system, Flatpak, Snap, and other supported locations.
- Preserved explicit `SOUNDAR_CODEX_BIN` and `CODEX_BIN` overrides for intentional testing while preventing stale PATH entries from hiding GPT-5.6 Sol, Terra, and Luna.
- Updated the development preview and reasoning selector contract for the live GPT-5.6 Sol, Terra, and Luna catalog, including Max and Ultra reasoning where the server offers them.

## 0.5.0 - 2026-08-27

- Added a dockable Creative Producer powered by the user's existing Codex CLI and Codex-owned ChatGPT login, without bundling Codex or reading its credential files.
- Added broad Linux Codex discovery, persistent app-server conversations, live model and reasoning selectors, read-only, Studio, and Full access modes, streaming messages, approvals, plans, and saved conversation history.
- Connected Codex dynamic tools to soundAr's real local state and durable speech, music, batch, project, job-inspection, and cancellation workflows with server-side read-only enforcement.
- Added end-to-end creative guidance that can research and shape incomplete goals, draft scripts, lyrics, directions, and project structures, execute local generation, and preserve intent through revisions.
- Added inline playable generated-audio artifacts with seeking, export, and revision follow-ups directly in the assistant conversation.
- Added responsive split-pane, overlay, and mobile layouts while preserving the user's explicit sidebar state and the neutral light-first design system.

## 0.4.1 - 2026-08-27

- Preserved the runtime-owned model catalog path across development, Debian, and AppImage upgrades so stale checkout paths cannot hide newly installed models.
- Kept ACE-Step Studio verified after its pinned runtime synchronizes two model-side Python files by accepting only the exact qualified source hashes.
- Qualified the recommended ACE-Step 1.5 Studio checkpoint through consecutive cold and warm native-bridge music generations on a 12 GB RTX 4080.
- Restored native resizing for the frameless Linux window and made speech and music workspaces fluid at full-screen widths while reflowing cleanly in compact desktop windows.
- Refreshed the GitHub presentation with the selected soundAr icon, current Music Studio workflows, local-first architecture, installation guidance, and synchronized model and release documentation.

## 0.4.0 - 2026-08-27

- Added the pinned official ACE-Step 1.5 Studio runtime with the 2B Turbo song model, local 1.7B planner, verified source archive, isolated dependencies, and automatic CPU offload for 12 GB GPUs.
- Rebuilt Music generation around Song, Instrumental, Extend, and Edit region workflows with structured Intro, Verse, Pre-chorus, Chorus, Bridge, Instrumental, and Outro sections.
- Added section regeneration, reference and source audio conditioning, cover and repaint controls, explicit reference-audio consent provenance, and editable synchronized lyric timing.
- Added one, two, or four concurrent variations with persistent inline playback, measured waveforms, seeds, model and timing metadata, and direct Extend, Remix, Edit, Keep, and output actions.
- Added multi-track stem extraction for vocals, drums, bass, and other layers through ACE-Step Base Tools, with grouped playable stem results in Generate.
- Replaced generic progress with Prepare, Plan, Render, Decode, and Finish stages, local-slot capacity, ETA, GPU-memory feedback, cancellation, retry, and completed results that remain available without opening History.
- Added a pre-install Music Studio setup experience that discloses download size, hardware fit, expected speed, license, access, installed state, and capabilities for every available music model.
- Added Rust, Python, React, provenance, responsive, visual, and isolated-runtime qualification coverage for the complete music workflow.

## 0.3.2 - 2026-08-26

- Added Breeze TTS 2 and Fish Speech 1.5 as pinned, isolated local synthesis engines with real RTX 4080 qualification evidence.
- Added local text-to-music foundations and expanded packaged engine resources.
- Reworked the desktop shell around a compact structural title strip, with page titles and actions kept inside each workspace and the sidebar left open until the user explicitly collapses it.
- Rebuilt the cross-platform icon family from the selected soundAr mark, including transparent Linux, Windows, macOS, Android, browser, loading, and About variants.
- Refined About into a centered product and local-runtime overview using the same restrained settings-row anatomy as the rest of the application.
- Added a deliberate compact music workspace: the composer takes the full working width, Runtime and Activity share the next row, and the information control stays in the title actions.
- Added manual update checks to Settings and About and synchronized the displayed application version with package metadata.

## 0.3.0 - 2026-08-13

- Replaced vulnerable Transformers and Diffusers pins with qualified patched releases, migrated XTTS to maintained `coqui-tts`, isolated optional engine dependencies, and disabled model-supplied Python execution.
- Added content-addressed foundation runtime upgrades so an application update cannot silently retain a stale shared Python environment.
- Fixed clean Linux CI builds by installing the ALSA development package required by native capture and playback tests.
- Removed development preview simulations and fixture data from production frontend output with compile-time guards and a negative-tested CI/package boundary verifier.
- Added an owned test matrix and executable release checklist covering source, UI, native, Python, package migration, GPU, physical audio, privacy, updater, and rollback evidence.
- Added five-cycle worker crash/reopen/retry injection and an isolated offline Debian/AppImage upgrade journey that verifies schema backup and user profile preservation.
- Rebuilt the `0.3.0` Debian/AppImage candidates and passed package inspection, offline `0.2.5` upgrade/preservation, and packaged RTX 4080 GPU acceptance.
- Added a configurable packaged GPU model-switch soak with WAV decoding, lifecycle unloads, scheduler leak checks, NVIDIA VRAM sampling, deterministic OOM quarantine/recovery, and machine-readable release evidence.
- Passed a `0.3.0` AppImage candidate through a 30-minute Kokoro/Whisper/Parakeet switch soak with stable idle VRAM, zero real engine failures, zero final scheduler reservations, and successful controlled OOM recovery.
- Passed real default-device capture and silent routed playback on the local PipeWire digital microphone and laptop speaker; hot-unplug and long Live-session checks remain manual release gates.
- Kept Play visible while consolidating secondary Voice, Model, and History table actions into compact accessible three-dot menus with sticky responsive action columns.
- Added durable parallel rendering for stale Project chapters with per-chapter model, voice, and language settings; compact pause, resume, retry, and cancel controls; restart reconciliation; and source-revision checks that never attach an outdated result after text or generation settings change.
- Validated batch reference audio against managed, consent-backed, ready voice profiles before either ordinary or idempotent batches are persisted, including default and row-level overrides.
- Added explicit scheduler-managed model load and unload actions, resident-model health and bootstrap state, scoped worker retirement, and active-engine mutation guards.
- Made model loading a durable cancellable job, preserved terminal cancellation through late worker responses, and excluded intentional cancellation from worker-failure telemetry.
- Quarantined failed model loads and CUDA OOM workers, and added measured VRAM-pressure reclamation with 512 MB cold-load headroom for large engines.
- Extended the real RTX 4080 acceptance run through Kokoro load, health, unload, multi-engine switching, API, parallel batch, and comparison; fixed hardware-test cleanup after assertion failures.
- Kept the expanded Models inspector inside ordinary laptop windows and revalidated all 174 route/theme/viewport Playwright cases.
- Replaced table-row action stacks with a shared keyboard-accessible three-dot menu. History keeps only Play visible while secondary artifact actions move into the menu; Models exposes details, install or repair, and source actions there.
- Added local WavLM speaker separation over measured transcript word windows, with GPU-aware scheduling, durable evidence, editable append-only speaker labels, and playable speaker turns.
- Added pinned English Wav2Vec2 forced alignment for corrected transcript revisions, with immutable source timing, uncalibrated acoustic-path disclosure, compact word playback, and stale-revision protection.
- Added explicit diarization limitations for provisional clustering, unavailable overlap detection, and unavailable turn confidence.
- Expanded responsive coverage to 174 Playwright cases across six viewport profiles down to 320 px, including cross-route collision checks, and added real RTX 4080 generation, transcription, diarization, and alignment proofs.
- Added a configurable route/theme soak suite and explicit Live output-disconnect reporting; a 25-cycle, 550-state local pass completed without clipping, viewport widening, console errors, or page exceptions.
- Added durable four-level queue priority with FIFO ordering, starvation-preventing aging, scheduler telemetry, and priority-aware parallel batch execution across the desktop UI, local API, CLI, CSV, and JSONL imports.
- Hardened cancellation at GPU worker admission so cancelled jobs cannot be revived and warm Python workers cannot be orphaned during the handoff.
- Fixed About-screen clipping for the version badge and long GPU names, and expanded responsive tests to cover definition rows and right-edge text.

- Added a bounded GPU-aware worker pool for concurrent interactive synthesis.
- Added durable parallel batch queues with pause, resume, retry, cancellation, and restart recovery.
- Added durable low/normal/high/urgent queue priority for interactive jobs and batch rows, with FIFO ties, 30-second aging, and live waiter telemetry.
- Added validated TXT, CSV, and JSONL batch import with per-row model settings, compact previews,
  and deterministic retry-safe output filenames.
- Added idempotent asynchronous speech and batch API endpoints, resumable job events, verified audio retrieval, and matching CLI commands.
- Added isolated pinned runtimes and qualification coverage for SpeechT5, Chatterbox, Chatterbox Turbo, XTTS, Whisper, and Parakeet alongside Kokoro.
- Added persistent projects, voice references and consent evidence, generation artifacts, history, comparisons, benchmarks, and application settings.
- Expanded desktop, mobile, dark, and cream-light UI coverage and improved compact table scrolling.
- Added clean-package checks for every engine resource, executable developer tooling, playback policy, and accidental model-weight inclusion.

## 0.2.5 - 2026-08-12

- Made release checksums portable after downloading assets from GitHub.
- Added a clean-room download and checksum verification gate before release publication.

## 0.2.4 - 2026-08-12

- Opened soundAr under the MIT License with contribution, security, and model-license guidance.
- Hardened repository ignores, CI permissions, issue templates, and dependency updates.
- Added guarded signed releases with source tests, package inspection, checksums, provenance, and draft verification.
- Replaced mutable runtime-manager bootstrapping with a pinned, checksum-verified uv download.

## 0.2.3 - 2026-08-11

- Allowed generated `blob:` audio previews in the packaged desktop security policy.
- Added audio header validation before handing generated files to the media decoder.
- Connected History play controls to generated files with loading, pause, and error states.

## 0.2.2 - 2026-08-11

- Added in-app setup when a direct package install has no managed Python runtime.
- Bundled one idempotent runtime bootstrapper for the app and Linux installer.
- Added visible setup progress, retry handling, and synthesis readiness guards.
- Added CUDA and CPU-specific PyTorch installation paths.
- Moved Kokoro English language data into setup to avoid a first-generation download.
- Promoted required Debian runtime tools from recommendations to dependencies.
- Added signed GitHub Release update checks with AppImage install-and-restart support.

## 0.2.1 - 2026-08-11

- Added the soundAr symbol and wordmark across the desktop interface.
- Added dark, light, white, and single-color brand asset variants.
- Added a responsive About view with application and local runtime details.
- Rebuilt desktop, mobile, and installer icons from the new master artwork.

## 0.2.0 - 2026-08-11

- Rebuilt the desktop experience with React, Vite, and Tauri.
- Added real local synthesis through a persistent Python model worker.
- Added generated-audio playback, seeking, and duration-scaled waveform progress.
- Added compact dark and cream-light interfaces across the workspace.
- Added managed Python 3.11 and CUDA 12.4 Linux installation.
- Added Debian, AppImage, CI, and tagged GitHub release automation.
