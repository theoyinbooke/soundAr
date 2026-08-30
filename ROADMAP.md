# soundAr Product and Engineering Roadmap

## Purpose

soundAr will become a local speech production and model-evaluation harness, not a
collection of model demos. It should let a user install, profile, route, compare,
compose, clone, dub, stream, and expose speech models through one dependable
desktop application.

This roadmap is ordered by dependency and product risk. New screens and model
names are deliberately later than persistence, execution, isolation, and testing.
That sequence prevents attractive but simulated features from being mistaken for
finished functionality.

The primary validation machine is an NVIDIA RTX 4080 Laptop GPU with 12 GB VRAM.
CPU-only behavior remains supported where practical, but every model must state
its actual hardware envelope instead of implying universal compatibility.

## Product Principles

1. **Local by default.** Inference, projects, references, and generated media stay
   on the user's machine. Network access is explicit and limited to downloads and
   metadata refreshes.
2. **Product truth.** The interface never displays invented metrics, fake progress,
   placeholder playback, or a successful state that was not reported by a real
   operation.
3. **Capability-driven controls.** The selected engine declares what it supports.
   Unsupported language, cloning, streaming, emotion, or SSML controls disappear
   or explain why they are unavailable.
4. **Reproducible output.** Every generation records model revision, engine
   runtime, settings, seed, voice references, hardware, and application version.
5. **Isolated engines.** A new model cannot destabilize established engines by
   changing their dependency environment.
6. **Measured quality.** Latency, RTF, memory, intelligibility, speaker similarity,
   and human preference are separate measurements. No unexplained composite
   "quality" number is presented as fact.
7. **Safe voice handling.** Voice ownership, consent, source files, and usage
   restrictions are first-class data, not a checkbox added after cloning works.
8. **No half-built production features.** Experimental work stays behind a
   development flag until it passes the definition of done in this document.
9. **User-controlled model acquisition.** Model weights are never bundled with the
   app or fetched implicitly. Every model download is a separate user-approved
   operation with source, revision, license, size, access, and hardware disclosures.

## Current Baseline

The `v0.3.0` release candidate contains the implementation status below. It becomes the released baseline only after every source, package, upgrade, signing, and downloadable-asset gate passes.

| Area | Current state | Product truth |
| --- | --- | --- |
| Linux packaging and updates | Local candidate verified | Debian/AppImage automation, signed updater metadata, checksums, provenance, resource inspection, and an isolated offline upgrade verifier exist. Fresh unsigned candidates pass package inspection plus prior-release launch, clean candidate launch, schema-29-to-30 backup/migration, and database/project/voice/model/registry/export preservation across Debian and AppImage. Privileged Debian install/uninstall, first-time runtime/model setup, and signed updater publication remain release gates. |
| Kokoro generation | Functional | Real local generation, parallel workers, WAV/FLAC playback, waveform progress, durable history, and runtime measurements pass GPU tests. |
| Additional TTS engines | Functional beta | SpeechT5, Chatterbox, Chatterbox Turbo, and XTTS now generate verified WAV artifacts through isolated CUDA workers in the native harness. Measured 4080 envelopes are recorded; XTTS admission is conservatively reserved at 10.8 GB after a measured 10.34 GB peak. Packaged clean-install qualification remains. |
| Models | Beta | Explicit, pinned, user-approved installs, repair checks, isolated runtimes, scoped load/unload controls, truthful resident-model state, and removals are implemented. Every enabled catalog model now has a 40-character revision; SpeechT5 also pins its vocoder and xvector dataset. No model weights ship with soundAr. |
| Voices | Beta | Managed originals, non-destructive reference revisions, analysis, corrected transcripts, consent evidence, and replayable per-model acceptance decisions are implemented and locally verified. Packaged import/edit/delete verification remains. |
| Durable core, jobs, and batches | Functional | Schema 30 SQLite state now includes typed restart-safe preferences, append-only job progress events, atomic checksum-backed artifact publication/recovery, restart-safe API speech and batch idempotency, exact cancellation, guarded rolling batch coordinators, bounded GPU-aware parallel workers, durable four-level queue priority with FIFO ties and starvation-preventing aging, graceful batch pause/resume, explicit retry, deterministic attempt-safe filenames, non-destructive queue clearing, durable History export receipts, benchmark and speaker-similarity evidence links, native worker-state timing, versioned transcription, diarization, and alignment evidence, append-only transcript corrections, and append-only speaker-label revisions. A five-row timing proof, live priority-admission proof, real two-row GPU/API batch, 10,000-row search fixture, concurrent reader/writer test, and this machine's legacy-schema backups pass. |
| Projects | Functional beta | Durable chapters/clips/revisions, TXT/Markdown/CSV/JSONL/SRT import, parallel stale-chapter rendering through the restart-safe scheduler, per-chapter model/voice/language settings, pause/resume/retry/cancel, source-revision-safe reconciliation, selective regeneration, and mastered WAV/FLAC export with provenance work. Timeline editing and office/ebook imports remain. |
| History and Compare | Functional beta | History has native search and model/voice/favorite/artifact filters, checksum-gated playback, metadata, regenerate, managed duplication, no-overwrite lossless export receipts, reveal, and safe delete. Play remains immediately available in each row while secondary actions use one compact accessible menu. Compare runs two-to-four take matrices in parallel, preserves partial failures, supports synchronized playback, blind reveal, ratings, notes, favorite, winner/tie, and promotion. Packaged clean-profile workflow and long-session review remain. |
| Benchmarks and routing | Beta | Real measured runs, transparent local routing, native-derived WER/CER, idle-safe cold/warm coordination, and consent-backed WavLM x-vector similarity now work. The runtime records end-to-end and startup overhead at worker acquisition; intelligibility and similarity scores link exact checksum-verified artifacts. Similarity remains explicitly comparative rather than identity proof. Long stability suites remain. |
| Live and transcription | Beta foundation | Native microphone capture, routed output-device playback, browser preview playback, Whisper transcription, provisional local speaker separation, and revision-linked English forced alignment work. Capture uses a bounded half-second queue, adaptive local VAD, configurable input gain, speech-gated silence auto-stop, dropped/buffered-frame telemetry, and recoverable device-error finalization. Routed playback validates managed WAVs, resamples to the selected output, and reports progress, startup time, and underruns. Live refreshes input/output inventories every two seconds, preserves valid selections, and falls back to a new default after disconnect. Opt-in speech cleanup produces a non-destructive derived WAV. Schema 30 persists versioned word timing, measured Whisper language probabilities, model-declared NeMo language, nullable confidence evidence, append-only text corrections that cannot alter measured timecodes, WavLM word-window clustering evidence, append-only speaker labels, and separate Wav2Vec2 CTC alignment evidence linked to the exact correction revision. The compact transcript UI exposes original word, aligned-word, segment, and speaker-turn playback plus correction and label revision controls. Diarization is explicitly provisional and does not detect overlap or report turn confidence; alignment is English-only and its acoustic scores are explicitly uncalibrated. Streaming synthesis, continuous monitoring, in-flight automatic reconnect, translation, and dubbing remain unavailable. |
| Developer harness | Beta | Explicit token-protected loopback API, OpenAPI, CLI, durable controls, rolling parallel batches, scheduler/VRAM telemetry, resumable SSE progress, cancellation, and restart-safe idempotent speech/batch jobs work. Chunked audio streaming remains. |
| Automated tests | Functional foundation | 58 reproducible Python contracts/audio tests, 37 React tests, 74 passing native Rust tests plus 4 opt-in hardware/package fixtures, 174 Playwright preview cases across six viewports, and one production-boundary browser check pass locally. Production output is compile-time stripped of preview simulation, byte-scanned in CI/packages, and behaviorally fails closed without Tauri. The rendered matrix navigates every route in dark and cream-light themes from 320 px phone width through full desktop, exercises dropdowns, dialogs, expanded editors, project and imported batch queues, comparison takes, compact table action menus, History actions, model residency actions, speaker-label editing, forced-alignment revision state, mobile navigation clearance, cross-control collisions, and deliberately hostile unbroken runtime errors. Project contracts prove that completed chapter audio is linked only to the exact submitted text/model/voice/language revision and that managed clone references are accepted while external paths are rejected before persistence. A separate configurable soak completed 25 route/theme cycles (550 rendered route states) without clipping, viewport widening, console errors, or page exceptions. Native recovery now includes five consecutive crash/persist/reopen/cleanup/retry cycles with no partial publication, leaked scheduler reservation, or lost attempt evidence. The real-GPU evidence covers a reserved cold/warm/warm Kokoro sequence, explicit Kokoro load/health/unload, VRAM-pressure reclamation before Parakeet, multi-engine inference, playback integrity, parallel requests, transcription with measured language probability, checksum-linked WER/CER, rolling API batch, comparison, History duplicate/export, WavLM similarity and diarization, and Wav2Vec2 forced alignment. The `0.3.0` packaged AppImage runtime acceptance completed in 70.59 seconds on the RTX 4080. The exact candidate also completed a 30-minute packaged model-switch/OOM soak: 92 Kokoro/Whisper/Parakeet cycles in 1,817.42 seconds, 9,744 MB peak system VRAM, identical 1,265 MB idle VRAM before and after, zero real engine failures, zero final scheduler reservations, and passing controlled OOM quarantine/recovery. Transcript tests prove append-only restart-safe corrections, speaker labels, and alignment evidence; timestamp immutability; stale-revision rejection; no-op deduplication; accessible editing; and compact dark/light layouts. Synthetic cleanup tests prove original-hash preservation, speech retention, and measured noise-floor reduction. Opt-in physical hardware fixtures passed on the default PipeWire digital microphone and laptop speaker, recording/decoding/deleting a real capture and completing silent routed playback. The isolated package journey verifies offline prior/candidate launches and a schema-29-to-30 upgrade preserving representative user data and files with a verified backup. Physical hot-unplug/reconnect and the two-hour Live audio soak remain release gates. |

Until Phase 0 is complete, prototype areas must be visibly marked experimental or
disabled in production builds. Seed data is acceptable in browser design preview,
but never in the installed application's runtime state.

The installed XTTS 0.22 / Transformers 4.49 combination was probed directly on this machine for
token-to-waveform streaming. Full synthesis remains qualified, but the upstream streaming generator
fails before its first chunk because it relies on older private Transformers generation internals.
The capability therefore stays locked until a pinned compatible implementation passes first-chunk,
cancellation, artifact integrity, and soak tests.

## Target Architecture

```text
React workspace
    |
    | typed Tauri commands and events
    v
Rust desktop core
    |- SQLite repository and migrations
    |- durable job scheduler
    |- artifact and path policy
    |- engine supervisor
    |- updater and desktop integration
    |
    | versioned local RPC
    v
Isolated engine workers
    |- one pinned uv environment per engine family
    |- capability manifest
    |- install / health / load / unload
    |- synthesize / stream / benchmark
    v
Managed local storage
    |- projects and database
    |- voice references
    |- generated artifacts
    |- model revisions
    |- logs and benchmark evidence
```

### Ownership Boundaries

- **React** owns interaction, visualization, accessibility, and optimistic UI only
  where an operation is safely reversible.
- **Rust** owns trusted filesystem access, persistence, job state, worker lifecycle,
  device coordination, update behavior, and public local APIs.
- **Engine workers** own model-specific Python dependencies and inference logic.
- **SQLite** is the source of truth for durable metadata. Media and weights remain
  files referenced by immutable IDs and manifests.
- **The model catalog** describes discovery and compatibility but does not prove
  that a model is installed or healthy. Runtime inspection is authoritative.

### Core Records

The first database schema should include:

- `projects`, `project_clips`, and `project_revisions`
- `voice_profiles`, `voice_references`, and `consent_records`
- `generations`, `generation_attempts`, and `artifacts`
- `jobs` and `job_events`
- `engine_runtimes`, `model_installs`, and `model_revisions`
- `presets`, `pronunciations`, and `app_settings`
- `benchmark_suites`, `benchmark_runs`, and `human_ratings`

Use SQLite migrations from the first schema. Enable WAL mode, foreign keys, and
transactional writes. Database rows store metadata; large audio and model files do
not become SQLite blobs.

## Delivery Method

### Work Unit

Every feature is delivered as a vertical slice:

1. Written user outcome and failure cases.
2. Data model or capability contract.
3. Backend implementation.
4. Desktop interaction and accessibility states.
5. Unit and integration tests.
6. Real GPU test when inference is involved.
7. Fresh-package test using the same artifact users download.
8. Documentation, migration notes, and rollback behavior.

A large milestone should be split into releasable slices. A model adapter, for
example, is one slice; three adapters should not share one untestable mega-PR.

### Definition of Ready

Work may start only when:

- the user outcome and non-goals are written;
- upstream model and code licenses have been reviewed;
- dependencies and required preceding milestones are identified;
- success metrics and representative fixtures exist;
- UI states include empty, loading, progress, success, cancellation, and failure;
- storage, privacy, migration, and offline behavior are understood.

### Definition of Done

A feature is done only when:

- no production path depends on seeded data, timers, or fabricated metrics;
- restart and cancellation behavior are tested;
- errors identify the failed component and give a useful recovery action;
- output and state survive an application restart where expected;
- automated tests cover the contract and main failure paths;
- the RTX 4080 12 GB hardware suite passes where relevant;
- a clean Debian or AppImage install passes the user workflow;
- upgrade from the previous release preserves user data;
- accessibility and dark/cream-light visual checks pass;
- logs contain diagnostics without text, audio, tokens, or personal paths unless
  the user explicitly requests a diagnostic export;
- release notes accurately distinguish stable, beta, and experimental behavior.

### Release Rule

No milestone is promoted by percentage complete. It either satisfies its exit gate
or remains unreleased behind a feature flag. Main-branch CI is necessary but not
sufficient for model releases; hardware and packaged-install evidence are also
required.

## Priority Roadmap

Effort ranges assume one focused senior engineer. They are planning ranges, not
promised calendar dates. Parallel work is appropriate only after shared contracts
are stable.

## Phase 0: Product Truth and Test Foundation

**Priority:** P0
**Expected effort:** 1-2 engineering weeks
**Why first:** We cannot safely expand a product while prototype behavior looks
indistinguishable from real execution.

### Deliverables

- Create a feature-state registry: `stable`, `beta`, `experimental`, or `disabled`.
- Remove simulated progress and random benchmark mutations from packaged builds.
- Disable or clearly mark Live, Compare, and Benchmarks until their real commands
  exist. Browser-only design preview may retain explicit fixture data.
- Add Python `pytest` coverage for audio utilities, model registry behavior,
  request validation, and bridge error serialization.
- Add Vitest and React Testing Library for UI state and typed bridge behavior.
- Add Playwright browser tests using a deterministic mock Tauri adapter.
- Expand Rust tests for IPC validation, path policy, worker failure, and response
  limits.
- Add test scripts to `package.json` and make CI run every first-party test suite.
- Add small, redistributable fixtures: silence, tone, clipped speech-like audio,
  malformed audio, and one consent-safe spoken phrase.
- Record baseline startup, generation, playback, and update smoke procedures.

### Required Tests

- Production build contains no call to simulated generation or benchmark code.
- Every navigation route renders at 820x620 and 1440x900 without overlap.
- Generate covers success, timeout, malformed response, invalid audio, cancellation,
  and worker-crash states.
- Offline launch succeeds when GitHub and Hugging Face are unreachable.
- CI fails when a production component imports fixture-only data.

### Exit Gate

- All first-party test suites run in CI.
- The installed app makes no false operational claim.
- Stable, beta, and unavailable features are unambiguous.
- A written test matrix exists and has an owner for each layer.

### Current Evidence

- Production preview branches are compile-time development-only. CI scans source
  guards and emitted assets, and a production-browser test proves a missing Tauri
  runtime shows the recovery screen without any fixture-backed workspace state.
- Python 3.11, React, Rust, and six-viewport Playwright suites run in CI. The owned
  evidence layers, triggers, commands, and limits are recorded in
  `docs/test-matrix.md`; `docs/release-checklist.md` owns the remaining human and
  privileged-system gates.
- Phase 0 product-truth and test-foundation requirements are implemented. Stable
  release publication still depends on the cross-phase package, GPU-soak, physical
  audio, updater-signing, and human-quality gates listed in the release checklist.

## Phase 1: Durable Local Core

**Priority:** P0
**Expected effort:** 2-3 engineering weeks
**Depends on:** Phase 0

### Deliverables

- Add SQLite with versioned migrations and transactional repositories.
- Replace voice `localStorage`, seeded benchmarks, and in-memory history with
  persisted records.
- Introduce a durable job state machine:
  `queued -> preparing -> running -> completed | failed | cancelled`.
- Persist progress events, attempts, errors, timestamps, and output artifact IDs.
- Create a managed artifact store with atomic temporary writes and checksum-backed
  finalization.
- Add History search, filtering, replay, reveal-in-folder, regenerate, duplicate,
  export, and safe delete.
- Add queue controls: cancel, retry, clear completed, and resume interrupted jobs.
- Add database backup before migration and repair guidance for corruption.

### Required Tests

- Migration from an empty install and each released schema fixture.
- Abruptly terminate the app during generation; the next launch marks or resumes
  the job correctly and leaves no valid-looking partial artifact.
- Concurrent readers and one writer do not corrupt history.
- Deleting a history record cannot delete a file outside the managed artifact root.
- Missing media is reported as missing, not silently removed from history.
- 10,000 history records remain searchable and the main table remains responsive.

### Exit Gate

- Voices, history, jobs, presets, and settings survive restart.
- Queue recovery passes repeated crash-injection tests.
- Database migration and rollback have been exercised on a copy of real user data.
- No React component is the sole owner of durable product data.

### Current Evidence

- Schema 30 migration, database integrity, legacy fixture migration, concurrent
  reader/writer, 10,000-record search, artifact damage, and path-boundary tests pass.
- History search and facets are evaluated by the native store; artifact-state
  filtering checks the file system instead of trusting stale UI state.
- Duplicate validates the source checksum, publishes a new managed artifact, and
  records its provenance. Export uses no-overwrite publication and a durable receipt.
- History controls and menus pass phone through desktop layouts in dark and cream
  themes. The action menu is viewport-positioned instead of being clipped by the table.
- Five consecutive crash-injection cycles persist failure, reopen the database,
  remove partial artifacts, retry with incremented attempts, publish checksum-backed
  WAVs, and finish with no leaked workers or scheduler reservations.
- The isolated package journey launches the prior and candidate Debian payloads
  offline, migrates a schema-29 profile with backup, and preserves representative
  database and file sentinels through Debian and AppImage. Remaining Phase 1
  release work is the privileged clean install/runtime/model/play/reopen/uninstall
  journey on the exact signed candidate.

## Phase 2: Engine Contract, Isolation, and GPU Scheduler

**Priority:** P0
**Expected effort:** 3-4 engineering weeks
**Depends on:** Phase 1

### Deliverables

- Define a versioned engine manifest containing engine version, task types,
  languages, speakers, cloning modes, streaming, accepted reference formats,
  controls, output formats, precision variants, minimum VRAM, license metadata,
  and health-test vectors.
- Replace the single shared runtime with one pinned `uv` environment per engine
  family. Shared model weights are allowed; shared incompatible Python packages
  are not.
- Implement worker RPC for `health`, `capabilities`, `install`, `load`, `unload`,
  `synthesize`, `cancel`, `stream`, and `benchmark`.
- Supervise workers from Rust with startup timeouts, heartbeat, bounded messages,
  structured logs, graceful shutdown, and forced cleanup.
- Add a GPU scheduler that serializes incompatible jobs, reserves expected VRAM,
  unloads idle models, and recovers memory after failures.
- Migrate Kokoro first and use it as the reference contract implementation.
- Expose real model installation, verification, repair, revision, disk usage, and
  removal from the Models screen.

### Required Tests

- A fake contract engine must pass the same suite as every real adapter.
- Kill a worker during load and inference; the scheduler recovers without an app
  restart or leaked GPU allocation.
- Install two engines with conflicting Transformers requirements and demonstrate
  that both remain functional.
- Corrupt or remove a model file and verify that health changes to `repair needed`.
- Cancel during download, model load, and synthesis without publishing partial
  artifacts.
- Exercise warm load, cold load, repeated model switching, and out-of-memory paths.

### Exit Gate

- Kokoro runs entirely through the new contract.
- Engine dependencies are isolated and reproducible from lock files.
- The UI is rendered from reported capabilities, not hard-coded engine names.
- A worker crash cannot crash or permanently wedge the desktop application.

### Current Evidence

- Versioned manifests, isolated pinned engine environments, bounded RPC, liveness
  probes, worker quarantine, OOM recovery, explicit model load/unload, and
  scheduler-scoped cancellation are implemented.
- Idle small-model workers stay warm when capacity permits. A cold large-model
  load reclaims idle GPU workers when measured free VRAM falls below its declared
  envelope plus 512 MB launch headroom; the RTX 4080 test now switches through
  Whisper and Parakeet without the prior capacity failure.
- Kokoro residency is visible from bootstrap and health, controllable from Models,
  and verified by a real CUDA load/health/unload cycle. Unloading is refused while
  the same engine owns active work, and failed loads are never recycled as warm.
- Model loading is represented by a durable job with visible progress and explicit
  cancellation. Cancellation cannot be overwritten by a late completion, terminates
  the assigned worker without recording a false crash, and leaves no scheduler or
  worker-pool allocation behind.
- Independent SpeechT5, Chatterbox, Chatterbox Turbo, Coqui, Transformers, NeMo,
  alignment, and speaker-verification environments coexist without dependency
  mutation.
- Repeated worker crash/reopen/retry injection passes, and the fresh packaged
  `0.3.0` AppImage runtime completes the qualified multi-engine GPU acceptance in
  70.59 seconds. The exact candidate then completed 92 packaged Kokoro, Whisper,
  and Parakeet model-switch cycles over 1,817.42 seconds. Peak system VRAM was 9,744
  MB; idle VRAM returned from 1,265 MB to the same 1,265 MB; every engine reported
  92 clean starts and zero failures; the scheduler finished with no active work or
  reservations; and deterministic OOM quarantine/recovery passed. Privileged
  first-time runtime/model setup on the exact signed candidate remains.

## Phase 3: Voice Lab

**Priority:** P1
**Expected effort:** 3 engineering weeks
**Depends on:** Phases 1-2

### Deliverables

- Record from a selected microphone or import WAV, FLAC, MP3, M4A, and OGG.
- Copy references into managed storage; never depend on an external file continuing
  to exist.
- Provide waveform trim, silence removal, channel conversion, resampling, and
  loudness normalization with non-destructive originals.
- Analyze duration, clipping, peak and integrated loudness, silence ratio, noise,
  sample rate, channel count, and speech activity.
- Transcribe reference content and let the user correct it.
- Support multiple references and engine-specific clone readiness.
- Record consent basis, speaker relationship, permitted uses, source date, and an
  immutable consent acknowledgement.
- Generate a fixed evaluation script after profile creation and allow the user to
  accept or reject readiness per engine.

### Required Tests

- Import every supported format, stereo and mono, short and long references, empty
  audio, corrupt files, clipping, and excessive silence.
- Verify original media remains unchanged after trimming and enhancement.
- A pending-consent profile cannot be used by cloning engines.
- Deleting a profile handles references used by existing generations without
  breaking their provenance.
- Compare the same reference through at least two clone-capable engines.

### Exit Gate

- A new user can create a real, consent-backed voice profile and use it after
  restarting the app.
- Readiness is derived from actual reference analysis and engine requirements.
- Preview, edit, archive, and delete all work from a packaged install.

## Phase 4: Generation Workbench, Takes, and Batch Queue

**Priority:** P1
**Expected effort:** 3-4 engineering weeks
**Depends on:** Phases 1-3

### Deliverables

- Make Text, SSML, and Batch modes real. SSML uses a normalized internal speech
  document and degrades explicitly when an engine lacks a requested control.
- Add language detection with manual override and compatibility validation.
- Expose only controls declared by the engine: speed, emotion, exaggeration, CFG,
  temperature, seed, pronunciation, duration, and reference mode.
- Generate a matrix of takes across models, voices, seeds, or settings.
- Add blind A/B playback, synchronized seeking, rating, notes, favorite, and
  `promote take` actions.
- Save reusable presets with schema versioning and capability validation.
- Durable queue priority with aging, validated TXT/CSV/JSONL batch import,
  pause/resume, retry, and per-row output naming are implemented.
- Export WAV, FLAC, MP3, and Opus with explicit sample rate, mono/stereo, loudness,
  metadata, and deterministic filenames.

### Required Tests

- Same seed and pinned model revision reproduce output within the engine's declared
  determinism contract.
- Unsupported controls cannot be silently ignored.
- A 1,000-row batch survives restart, cancellation, one bad row, and low disk space.
- Compare output labels remain hidden during blind review and are revealed later.
- Exported files decode in ffmpeg and contain expected duration and metadata.
- Long text segmentation avoids missing or duplicated text at boundaries.

### Exit Gate

- Compare executes real jobs and stores every take and rating.
- Batch jobs are recoverable and never block interactive cancellation.
- Presets remain valid or explain incompatibility after an engine upgrade.

## Phase 5: Benchmark Laboratory and Model Router

**Priority:** P1
**Expected effort:** 3 engineering weeks
**Depends on:** Phases 2 and 4

### Deliverables

- Replace all seeded values with reproducible benchmark suites and immutable runs.
- Measure cold load, warm load, time to first audio, total inference time, RTF,
  peak VRAM, system RAM, output duration, and failure rate.
- Add intelligibility checks by transcribing generated speech and calculating WER
  or CER against normalized source text.
- Add speaker-similarity scoring for cloned voices, clearly labelled as a proxy. Implemented with user-installed, pinned WavLM x-vectors and checksum-linked Voice Lab evidence; soak and cross-condition qualification remain.
- Track clipping, loudness, silence ratio, duration error, and repeated/hallucinated
  content.
- Add blind human listening ratings for naturalness, similarity, pronunciation,
  emotion, and preference.
- Store machine profile, driver, CUDA, precision, model revision, warm/cold state,
  and application version with every run.
- Build a transparent rule-based router for intents such as Fast, Natural,
  Expressive, Clone, Multilingual, and Duration matched.

### Required Tests

- Run the same suite three times and display variance rather than false precision.
- Verify measurements against external process timing and `nvidia-smi` sampling.
- A failed or partial run cannot rank above a completed run.
- Router decisions include a readable explanation and can be overridden.
- Benchmarking one engine cannot modify another engine's recorded result.

### Exit Gate

- Benchmark numbers come from real artifacts and can be reproduced from their
  manifests.
- No single unexplained `quality` score remains in the stable UI.
- Routing recommendations are derived from this machine's measured evidence.

## Phase 6: Model Expansion Wave One

**Priority:** P1
**Expected effort:** 4-6 engineering weeks, released one adapter at a time
**Depends on:** Phases 2 and 5

### Deliverables

Implement and qualify adapters in this order:

1. **Chatterbox Turbo** for low-latency English cloning and paralinguistic tags.
2. **Chatterbox Multilingual V3** for cross-language cloning and broad language
   coverage.
3. **Qwen3-TTS 0.6B** for streaming, expressive synthesis, cloning, and voice
   design.
4. **CosyVoice 3 0.5B** for multilingual zero-shot and instruction-controlled
   synthesis.

F5-TTS, Dia/Dia2, and IndexTTS2 remain candidates for the next wave. They enter
only after a use case, 12 GB profile, maintenance status, and license review are
documented. A passing demo is not an admission criterion.

### Adapter Admission Gate

Every model must provide:

- reviewed code and weight licenses, commercial-use notes, attribution, and known
  restrictions;
- pinned engine environment and model revision;
- resumable installation, revision verification, disk estimate, repair, and clean
  removal;
- offline inference after installation;
- capability manifest and unsupported-control tests;
- a consent-safe smoke corpus covering punctuation, numbers, acronyms, names,
  long text, silence, and each advertised language class;
- cold/warm metrics, 30-minute stability loop, cancellation, model switching, and
  out-of-memory recovery on 12 GB VRAM;
- reference-quality guidance and clone similarity checks where applicable;
- packaged Debian and AppImage end-to-end proof;
- upstream version monitoring without automatic untested model upgrades.

### Required Tests

- Run the complete adapter contract suite, consent-safe smoke corpus, offline test,
  cancellation test, model-switch test, and packaged-install test for each engine.
- Repeat cold and warm generation across all advertised modes and representative
  languages; unsupported combinations must fail before model execution.
- Complete a 30-minute stability loop while recording latency, failures, RAM, and
  VRAM, followed by a second engine load to prove resources were released.
- Upgrade and roll back the isolated engine environment without changing another
  engine or invalidating prior generation provenance.
- Compare the candidate against the current default in blind human review before
  making it a recommended route.

### Exit Gate

- Each adapter ships independently after its admission report passes.
- At least one fast, one expressive, and one multilingual/clone route is reliable.
- Installing or upgrading one adapter cannot change another adapter's environment.

## Phase 7: Project Studio and Long-Form Production

**Priority:** P2
**Expected effort:** 4-6 engineering weeks
**Depends on:** Phases 1, 3, and 4

### Deliverables

- Add projects containing scripts, chapters/scenes, speaker assignments, clips,
  takes, markers, and exports.
- Import TXT, Markdown, DOCX, EPUB, PDF, CSV, JSONL, SRT, and screenplay-like text.
- Preserve source structure and show import warnings rather than flattening content
  silently.
- Add a compact multitrack timeline with real waveform data, snapping, zoom,
  selection, split, merge, move, trim, fades, and keyboard undo/redo.
- Regenerate one sentence or clip while preserving surrounding material.
- Add track defaults with clip-level overrides for voice, model, language, and
  expression.
- Cache unchanged clips so editing one line does not regenerate an entire project.
- Export a mastered file, chapters, individual clips, stems, SRT, CSV, and a
  portable project archive.

### Current Evidence

- Written stale chapters queue concurrently through the same durable, GPU-aware
  scheduler as Batch and the local API, with a configurable worker count.
- Each row preserves its own model, voice, language, reference, seed, and output
  identity. Managed clone references are validated before the batch is stored.
- Project-to-batch linkage survives restart and reconciles completed History
  artifacts only when chapter text, model, voice, and language still match the
  submitted revision. Editing in flight leaves the newer chapter stale.
- Pause, resume, retry failed, and cancel are available without exposing a stack
  of row actions. A queued chapter cannot also be rendered manually.
- The remaining Phase 7 work is the timeline, broader document import, portable
  project archives, stems and subtitle exports, and one-hour interruption proof.

### Required Tests

- Import representative documents including malformed and very large inputs.
- Generate a one-hour project, interrupt it, resume, edit one clip, and prove only
  stale clips regenerate.
- Undo/redo remains correct across text edits and timeline operations.
- Project archive round-trip preserves hashes, references, settings, and media.
- Mixed sample rates and channels produce a valid, synchronized final export.

### Exit Gate

- A long-form project can be created, closed, reopened, revised, and exported
  without hidden regeneration or lost edits.
- Timeline operations are non-destructive and backed by project revisions.

## Phase 8: Real-Time Studio

**Priority:** P2
**Expected effort:** 4-6 engineering weeks
**Depends on:** Phases 2, 3, and a proven streaming adapter from Phase 6

### Deliverables

- Implement native audio device enumeration, capture, playback, and hot-plug
  recovery. Prefer Rust audio I/O with bounded ring buffers.
- Add input gain, VAD, noise suppression, monitoring, and output device routing.
- Implement streaming TTS and speech-to-speech/voice conversion as distinct modes.
- Display measured capture, model, buffering, and output latency components.
- Add interruption, flush, cancel, reconnect, and back-pressure behavior.
- Add PipeWire virtual microphone routing after direct monitoring is stable.
- Save optional session recordings only after explicit user consent.

### Performance Targets

- No unbounded audio queue growth.
- Zero dropouts in a 30-minute soak test under the declared hardware profile.
- Measured p50 and p95 latency displayed; no hard-coded latency values.
- Target first-audio latency below 250 ms for the fast route, with the exact model
  and buffer configuration recorded. If the hardware cannot meet it, report the
  measured result rather than masking it.

### Required Tests

- Device disconnect/reconnect, sample-rate mismatch, no microphone permission,
  worker restart, GPU pressure, cancellation, and feedback-loop prevention.
- 30-minute and two-hour soak tests with underrun/overrun counters.
- Verify virtual microphone output in at least two common Linux applications.

### Exit Gate

- Live mode passes soak testing and publishes real latency percentiles.
- Stop always releases microphone, worker, and virtual-device resources.
- The app remains usable after a worker or device failure.

## Phase 9: Transcription, Alignment, and Local Dubbing

**Priority:** P2
**Expected effort:** 6-8 engineering weeks
**Depends on:** Phases 3, 6, and 7

### Deliverables

- Add real transcription using a fast Whisper path and at least one accuracy route.
- Add VAD, language detection, word timestamps, confidence, and optional forced
  alignment. Language detection, native model word timestamps, and optional English
  CTC forced alignment are implemented with versioned evidence; confidence remains
  nullable when an adapter does not expose it, and alignment path scores are not
  presented as calibrated confidence.
- Add local speaker diarization with explicit model access/license setup and
  telemetry disabled by default where supported. Implemented as a public,
  pinned WavLM x-vector baseline over measured word windows; it is explicitly
  provisional and does not detect overlapping speakers or report turn confidence.
- Build editable speaker-labelled transcripts linked to source timecodes. Timed
  text corrections and speaker labels now persist as separate append-only
  revisions while model timestamps remain immutable.
- Add translation as an isolated, optional local engine with language-pair quality
  disclosure.
- Create per-speaker voice profiles or assign existing voices.
- Generate duration-constrained clips, preserve background audio where possible,
  and flag segments that cannot fit naturally.
- Export remixed audio/video, dialogue stems, SRT, CSV, and project archives.

### Required Tests

- Single speaker, multiple speakers, overlapping speech, music, noise, silence,
  code switching, and long recordings.
- Timestamp drift is measured at the beginning, middle, and end of long media.
- Every translated line remains linked to source text and can be manually corrected.
- Duration fitting never silently truncates spoken content.
- Video exports retain expected duration, frame rate, and audio synchronization.

### Exit Gate

- A multi-speaker source can move from import to an editable local dub and valid
  export without cloud inference.
- Low-confidence transcription, diarization, translation, and fit decisions are
  visible and correctable.

## Phase 10: Developer Harness

**Priority:** P2
**Expected effort:** 3-4 engineering weeks
**Depends on:** Phases 1-6

### Deliverables

- Add a localhost-only OpenAI-compatible `/v1/audio/speech` endpoint.
- Add native endpoints for capabilities, voices, jobs, batches, benchmarks, and
  streaming audio.
- Provide WebSocket streaming, cancellation, idempotency keys, and structured
  errors.
- Add a `soundar` CLI for status, install, generate, batch, benchmark, and serve.
- Publish a versioned OpenAPI document plus small Python and TypeScript clients.
- Require an explicit opt-in and local token before binding beyond localhost.
- Add request limits, path sandboxing, origin controls, and concurrency policy.

### Required Tests

- OpenAI client compatibility, malformed requests, oversized payloads, traversal,
  cancellation, duplicate idempotency keys, worker failure, and rate limits.
- API and desktop jobs share the same durable queue and provenance.
- Remote binding is disabled by default and cannot occur accidentally.

### Exit Gate

- A third-party local application can discover capabilities, generate, stream,
  monitor, and cancel without depending on soundAr's internal database schema.
- API compatibility and security tests are release blockers.

## Phase 11: Audio Finishing, Pronunciation, and Trust Tooling

**Priority:** P3, with consent controls delivered earlier in Phase 3
**Expected effort:** 4-6 engineering weeks

### Deliverables

- Pronunciation dictionaries for names, brands, acronyms, heteronyms, numbers,
  phonemes, and language-specific substitutions with instant audition.
- Non-destructive finishing chain: trim, fades, normalization, loudness target,
  limiter, EQ presets, de-essing, compression, and resampling.
- Optional generated-audio watermark integration where an engine supports it.
- Provenance manifests and export-side disclosure metadata.
- Voice-use audit view and revocation/archival workflow.
- Optional local watermark/deepfake inspection clearly described as probabilistic,
  never as proof of identity or authenticity.

### Required Tests

- Apply and bypass every finishing operation, reopen the project, and reproduce
  byte-identical output where the processing chain declares determinism.
- Verify pronunciation rules at project, voice, and global scope without leaking
  one project's private dictionary into another.
- Preserve the unmodified source artifact through repeated finishing edits and
  restore it without quality loss.
- Confirm provenance exports match the actual model revision, voice references,
  processing steps, and generated artifact checksum.
- Evaluate watermark and deepfake tools on known positive, negative, transcoded,
  and adversarial samples; report uncertainty and measured false results.

### Exit Gate

- Finishing is reproducible from saved parameters and can always be bypassed.
- Trust indicators distinguish recorded fact, user assertion, model metadata, and
  probabilistic detection.

## Phase 12: Narrative Production - Cast, Score, and Show Formats

**Priority:** P2, delivered after the Video Studio timeline contract is stable
**Expected effort:** 10-14 engineering weeks across ten releasable slices
**Depends on:** Phases 1, 3, 4, 7, and the durable Video Studio manifest
**Why now:** soundAr can already synthesize speech, compose music, and render a
captioned local video, but a user who wants one finished story episode must still
drive every primitive by hand. The missing work is not inference capability. It is
the small set of durable abstractions - a cast, a performance clock, a score, and a
reusable format - that turn a studio into something a person can publish from every
week.

**Non-goals for this phase:** generative sound-effect models, video avatars, cloud
rendering, collaborative editing, and any melody-conditioned music route. Sound
design in this phase is user-supplied local media, not generated audio.

### Product Outcome

A user describes an episode. soundAr assigns the cast, writes and revises the
script, performs it with distinct consent-backed voices and believable turn timing,
scores it with a fitted bed and a resolving outro, renders it as a captioned local
video, checks the result against the script it was asked to speak, and publishes one
release containing the audio episode, the video, a short vertical trailer, a
transcript, and show notes.

### Slice 12.1: Cast and Dialogue Script

Depends on: Phase 3 voice consent, Phase 7 project revisions.

- Add a durable `Cast` to the project manifest: named characters, each bound to a
  voice reference, model, language, and delivery defaults, with the same consent
  evidence and revision rules as a managed voice reference.
- Accept a speaker-attributed script - `SPEAKER: line` with optional parenthetical
  direction - and parse it into ordered `DialogueTurn` records that preserve source
  line numbers and reject unknown speakers instead of silently narrating them.
- Replace scene-scoped narration with turn-scoped narration. Every turn carries its
  own `NarrationBinding`, so one re-read of one line invalidates only that turn.
- Cast membership, per-character delivery, and turn text are separately revisable
  and each records which rendered takes it invalidated.

### Current Evidence

Slice 12.1 is implemented and locally verified.

- `video/cast.rs` holds the `CastMember`, `CastDelivery`, and `DialogueTurn` contracts and the
  strict script parser. A speaker header must name a declared character or be an all-capitals
  screenplay cue, so prose containing a colon continues its turn while a misspelled character
  name is reported instead of being narrated by the wrong voice.
- `video/dialogue.rs` applies a cast and script as a pure, revision-safe manifest transformation.
  A turn's identity is its character and its words, so re-applying a script keeps every unchanged
  turn's identifier and its rendered take. Reordering costs nothing, a repeated line keeps both of
  its turns, and changing only a stage direction does not discard a valid take.
- The manifest carries `cast`, `dialogue`, and turn-scoped `narration_bindings`. Validation
  enforces that a take records the character who speaks its turn, matches that turn's exact words,
  and is unique per turn. Legacy manifests decode with an empty cast and dialogue.
- `VideoStudioService::apply_script` commits through the same durable job, project lock, revision
  CAS, and idempotent replay path as `edit_timeline`, and its receipt separates retained turns from
  new ones so the caller renders only what changed.
- The `write_video_script` agent tool, its Tauri command, the headless `agent video` dispatch, the
  presented project shape, and the typed frontend bridge all use that one service operation.
- Verified locally: 338 native tests including cast-parser, manifest-contract, and
  dialogue-application cases plus one durable service case proving version binding, idempotent
  replay, stale-write rejection, and turn reuse; and 142 React tests including preview-bridge
  coverage of the same reuse behaviour.
- Remaining for this slice: the desktop cast and script editing surface, and per-turn narration
  rendering through the scheduler.

### Slice 12.2: Performance Timing

Depends on: 12.1.

- Add a `PerformanceClock` describing the gap policy between turns: intra-exchange,
  turn-of-thought, pre-reveal beat, and scene boundary.
- Derive default beats from terminal punctuation and parenthetical direction, and
  allow an explicit per-turn override that survives a script edit that did not
  change that turn's words.
- Support declared overlap for interjections, bounded so that an overlap can never
  reorder turns or exceed the shorter turn.
- Continuous room tone per scene is delivered in 12.5, where registered sound-design
  media exists to supply it. Beats and overlap ship here without it rather than
  shipping a control that cannot yet produce sound.

### Current Evidence

Slice 12.2 is implemented and locally verified.

- `video/performance.rs` holds the `PerformanceClock` and `TurnBeat` contracts and derives a beat
  for every turn from what the script already says: a reply is faster than the same character
  continuing a thought, a line that trails off or carries a pause direction earns a longer beat,
  and an interjection direction overlaps the previous line instead of waiting.
- An explicit beat is keyed by turn id, so a deliberate pause survives every later edit that leaves
  its own line alone and is discarded only with the line it belongs to. A stale derived beat is
  always recomputed and is never mistaken for an override.
- A turn may hold a lead-in or overlap the previous turn, never both. An overlap is rejected on the
  first turn, and bounded against the shorter of the two rendered takes - measured from published
  artifacts, never guessed when a take does not exist yet.
- `set_turn_beat` and `clear_turn_beat` join the existing revision-checked `edit_video_timeline`
  batch rather than adding a parallel path, and are exposed to the assistant through that tool.
- Retiming invalidates only assembly: `/turn_beats` and `/performance_clock` map to scene render,
  preview, final render, and publish, never to Speech. Moving a pause never re-reads a line.
- Beat validation runs inside `validate_strict`, which is on the hot path for every manifest load,
  revision, and render admission, so it uses one prebuilt turn index instead of scanning the
  narration bindings per beat.
- Verified locally: 359 native tests, including twelve beat-derivation and bounds cases, four
  editor cases proving an explicit beat is marked, restored, and never invalidates speech, and a
  two-thousand-turn manifest that validates and still names a bad beat deep inside it; plus 143
  React tests including preview-bridge parity for derivation and for a held pause surviving a
  later edit.

### Slice 12.3: Project Pronunciation Lexicon

Pulled forward from Phase 11 because invented names make it a prerequisite for
stories, not a finishing touch.

- Project, cast-character, and global lexicon scopes with explicit precedence.
- Entries apply to every take and every voice in the project and are recorded in
  the take's reproducibility metadata.
- Instant audition of a single entry without rendering the surrounding work.
- One project's lexicon never leaks into another.

### Current Evidence

Slice 12.3 is implemented and locally verified.

- `video/lexicon.rs` holds the `LexiconEntry` contract, precedence resolution, application, and
  fingerprinting. Precedence runs character, then project, then global, and within a scope the
  longest match wins so a rule for a full name is not consumed by a rule for its first word.
- A rule never fires inside a longer word, so a rule for `Ada` cannot mispronounce `Adaeze`.
  Replacement text is final: a lower-precedence rule cannot rewrite inside it, which keeps the
  spoken result predictable from reading the lexicon.
- The project's lexicon is self-contained. Entries imported from the machine's global lexicon are
  snapshotted into the project with `Global` scope, so an episode reproduces identically later even
  if the global lexicon has since changed, and one project's rules can never reach another.
- A take records the fingerprint of the exact rules that produced it. Changing a rule stales
  exactly the lines that rule governs: a character-scoped rule drops only that character's takes,
  and a rule no line uses changes no fingerprint and re-reads nothing. Takes recorded before the
  lexicon existed carry no fingerprint and stay valid.
- `set_lexicon_entry` and `remove_lexicon_entry` join the existing revision-checked
  `edit_video_timeline` batch and are exposed to the assistant through that tool.
- `preview_video_pronunciation` resolves exactly what a voice would say for a sample line, so one
  rule can be auditioned by synthesizing one short line with the character's own voice rather than
  re-rendering the work around it.
- Replacements are ordinary respelled text rather than a phoneme alphabet, because soundAr's
  engines differ in what notation they accept and a rule that works on one engine only is worse
  than a respelling that works everywhere.
- Verified locally: 381 native tests including sixteen lexicon cases covering precedence,
  word boundaries, multibyte text, non-cascading replacement, and fingerprint isolation; three
  manifest contract cases proving a rule stales only the takes it governs; and three editor cases
  proving scoped drops. Plus 144 React tests including preview-bridge parity.

### Slice 12.4: Score Cue Sheet

Depends on: 12.1, existing `AudioMix` ducking and loudness contracts.

- Add a `CueSheet` of music cues, each with a role - `sting`, `bed`, `transition`,
  or `outro` - an anchor to a scene or turn, a target duration, and a direction.
- Fit generated music to its target duration using the existing ACE-Step extend and
  edit-region routes plus a musical tail fade, rather than a hard cut.
- Automatically bind a `bed` cue to a ducking envelope sidechained to the speech
  track, using the mix contract that already exists.
- An `outro` cue resolves after the final turn and defines the episode's end.

### Current Evidence

Slice 12.4 is implemented and locally verified, end to end.

- `video/score.rs` holds the `MusicCue` contract. A cue's role - sting, bed, transition, or outro -
  decides where it sits and how it is mixed, rather than treating four different jobs as one
  generic audio file stapled to the end of an episode.
- Cues anchor to a scene or a turn rather than to an absolute timestamp, so re-reading a line or
  retiming a pause moves the cue with it. Only an outro may anchor after the final line, only an
  outro may use that anchor, an outro needs a script to play after, and an episode may end on only
  one outro.
- A bed placed on a timeline track is given its ducking envelope automatically, sidechained to the
  audio track that actually carries narration rather than to a default the renderer would have to
  resolve. The manifest refuses a bed on a track that does not duck, and removing a cue takes its
  mix entry with it so no envelope is left pointing at music the project no longer has.
- `fit_cue` fits generated music to its target. A piece that runs long is trimmed with its fade-out
  extended to carry a musical tail rather than cut off, and the tail is bounded by half the cue so
  a short sting cannot become mostly fade. A piece that falls more than the tolerance short is
  reported for regeneration: stretching would change its tempo, and padding with silence would put
  dead air where the score should be.
- A cue cannot occupy a timeline track before its music exists, and can only reference a registered
  soundAr music artifact, so a planned cue never presents itself as rendered score.
- `set_music_cue`, `remove_music_cue`, and `place_music_cue` join the existing revision-checked
  `edit_video_timeline` batch and are exposed to the assistant through that tool.
- `generate_cue_music` is the durable job that closes the loop: it asks the installed local music
  model for the cue's exact target length, registers the result through the ordinary managed import
  so a cue can only point at bytes soundAr owns, fits it, and places it at its anchor. It uses the
  same parent/child idempotency as prompt-to-video, so a crash between composing and placing adopts
  the existing History rather than asking the GPU for the same piece twice, and the generation seed
  is derived from the direction so a resume produces the piece the user approved.
- Anchors resolve at placement time from the timeline as it stands, so a cue can be authored before
  the scenes and takes it refers to are final. An outro cannot be placed before there is narration
  to resolve after, and a cue that will not fit inside the timeline at its anchor is refused.
- Verified locally: 398 native tests including thirteen score cases covering roles, anchors, fit
  behaviour, and asset binding, plus four editor cases proving a bed receives its envelope, cannot
  be placed without narration to duck against, and takes its mix entry with it on removal. Plus 145
  React tests including preview-bridge parity.

### Slice 12.5: Sound Design Library

Depends on: 12.2 for placement, existing visual-asset registration for the pattern.

- Register user-supplied local audio as tagged sound-design assets through the same
  rights-and-registration path as visual assets. No generative model is introduced.
- Place one-shot effects at a turn or timeline position and ambience across a scene,
  both as revisable timeline layers with independent level and fades.
- The assistant may propose placements from parenthetical stage directions, but a
  placement is only applied through the same revision-checked service the UI uses.

### Current Evidence

Slice 12.5 is implemented and locally verified, end to end.

- `video/sound.rs` holds the `SoundAsset` and `SoundLayer` contracts. Nothing here generates audio:
  assets are files the user already has, carrying the same rights and provenance record as any
  other imported media, which keeps the feature outside the licensing and hardware-qualification
  questions a generative sound-effect model would raise.
- Room tone is delivered here, where registered media exists to supply it. It must run under the
  whole scene rather than stopping partway, because stopping leaves exactly the digital silence it
  exists to remove, and a scene has only one room and so only one room tone. Its level is capped
  far under the dialogue; nearer than that it reads as noise rather than as a room.
- A one-shot must be anchored to the scene or turn it punctuates, so a later edit cannot silently
  move it away from the moment it was placed for, and it cannot loop. Ambience and room tone belong
  to a scene rather than to one line. No placement may spill past its scene into the next cut.
- Assets are found by normalized tag rather than filename, because the assistant proposes
  placements from written stage directions rather than from a controlled vocabulary.
- Registration reuses the ordinary local-import pipeline rather than duplicating it. The media is
  imported as managed media - copied, checksummed, and probed by the proven path - and
  `register_sound_asset` then labels that managed source. A `SoundAsset` therefore carries only a
  name and its tags; duplicating the path, checksum, or duration would create a second copy of
  facts that could disagree with the source they describe. Because only the native import path can
  create a managed source, the assistant can never name an arbitrary file on the machine.
- Media with no audio track cannot become sound design, one managed source is registered as exactly
  one sound, and removing a sound removes its placements rather than leaving the manifest with a
  placement that has no audio.
- `register_sound_asset`, `remove_sound_asset`, `set_sound_layer`, and `remove_sound_layer` join the
  existing revision-checked `edit_video_timeline` batch. A placement can only reference an already
  registered asset, so a proposal from a stage direction can never invent audio the project does
  not have.
- The `edit_video_timeline` operation union outgrew a single `json!` literal and now builds from one
  schema function per operation.
- Verified locally: 415 native tests including thirteen sound cases and four editor cases, plus 146
  React tests including preview-bridge parity.

### Slice 12.6: Show Formats

Depends on: 12.1 through 12.5.

- Save a reusable `ShowFormat`: cast, caption preset, visual treatment, cue sheet
  defaults, opening and closing, loudness target, aspect ratio, target length, and
  show-notes style.
- Instantiate an episode from a format with one brief. Inherited values are visibly
  inherited and can be overridden per episode without editing the format.
- Editing a format never retroactively mutates a published episode.

### Current Evidence

Slice 12.6 is implemented and locally verified.

- `video/format.rs` holds the `ShowFormat` contract: the cast, pronunciation rules, conversational
  timing, caption preset, canvas, frame rate, loudness targets, usual episode length, show-notes
  style, and opening and closing cue templates that do not change between episodes.
- Instantiation copies. An episode never reads back through its format at render time, so editing a
  format cannot retroactively change an episode that already shipped, and an episode rendered next
  year reproduces what it was rendered from today. `format_origin` on the manifest records which
  format and which revision the values came from - provenance, not a live link.
- soundAr owns the format revision rather than the caller, because an episode records the revision
  it inherited and a caller that could choose its own number could make two different formats claim
  the same provenance. `created_at` is preserved across updates.
- A format cannot store values the renderer would later reject: the loudness target is validated by
  building the mix an episode would actually inherit, the caption preset must be one soundAr has, a
  cast whose script names cannot be told apart is refused, and a rule cannot name a character
  outside the format's own cast.
- Cue templates carry no anchor, because an opening belongs to whatever the first line turns out to
  be and a closing to whatever the last one is. `materialize_format_cues` resolves them against a
  written script and returns nothing for an episode that has none, rather than inventing an anchor
  at a moment the writer never chose. An opening cannot be an outro and a closing must be.
- Formats persist as one validated document in the durable settings table, which keeps them
  transactional and restart-safe without a schema migration, and they are validated on the way out
  so a corrupted document cannot reach a project as if it were a usable format.
- `save_show_format`, `list_show_formats`, and `create_episode` are exposed as assistant tools and
  Tauri commands, and the producer prompt now directs recurring work through them.
- The cast and lexicon tool schemas are shared between `write_video_script` and `save_show_format`
  rather than duplicated.
- Verified locally: 432 native tests including eight format cases and one durable service case
  proving the revision is owned by soundAr, an episode inherits by copy, and recasting the show
  leaves an existing episode untouched; plus 148 React tests including preview-bridge parity for the
  same guarantee.

### Slice 12.7: Release Package

Depends on: 12.6, the existing publish package and candidate analyst.

- Produce one release from one production: podcast audio with chapter marks and
  embedded metadata, the video master, a short vertical trailer, a square audiogram,
  the transcript, and show notes.
- Cut the trailer by running the existing candidate-moment analyst over the episode's
  own narration transcript, so the same reviewed-candidate contract applies to
  generated work as to imported source.
- Every release member is checksum-registered and playable in the application, never
  presented only as a filesystem path.

### Current Evidence

Slice 12.7's planning layer is implemented and locally verified. The remaining work is the FFmpeg
rendering of the audio episode with embedded chapters, the vertical trailer cut, and the audiogram.

- `video/release.rs` holds the release contract. A blocked member always names its missing
  prerequisite, because a release that quietly omits its trailer looks identical to one that never
  wanted a trailer.
- `episode_transcript` builds a source-clock transcript from the episode's own narration. A turn
  appears only when it has a published take with a measured duration and a clip placing it on the
  timeline, so the result is a measurement rather than an estimate and a moment chosen from it is
  the same moment in the finished file. An unperformed script yields no transcript at all rather
  than one describing audio that does not exist, and the timing source is recorded as written
  rather than claiming a recognizer produced it.
- That transcript is fed to soundAr's existing candidate analyst, so the trailer is chosen from
  generated work by the same deterministic rules already used on imported source, rather than by a
  second, unproven selector. This is the loop the original design called for: the moment-finder
  built for imported video, pointed at soundAr's own output.
- Scenes are the episode's chapters, because they are the author's own divisions. An episode with
  no scenes has no chapters rather than one invented per line.
- `plan_episode_release` is exposed as an assistant tool and a Tauri command, and the producer
  prompt now directs the assistant to call it before declaring an episode finished.
- Verified locally: 438 native tests including six release cases - one proving the existing analyst
  picks a bounded, on-clock moment from generated narration - plus 149 React tests.

### Slice 12.8: Production Quality Control

Depends on: 12.1, Phase 9 transcription and alignment evidence.

- Re-transcribe rendered narration and diff it against the exact script revision it
  was asked to speak, reporting skipped, inserted, and mispronounced words per turn.
- Measure integrated loudness and true peak against the format's target.
- Measure caption drift against the rendered word timings.
- Detect dead air and clipping.
- Report every finding as reviewable evidence linked to a turn. Quality control
  flags work; it never silently rewrites or re-renders it.

### Slice 12.9: Assistant Listening and Director's Pass

Depends on: 12.8.

- Add a strict read-only tool that returns what was actually rendered - transcript,
  word timings, per-speaker duration, loudness, and gap distribution - so the
  assistant revises from measured output instead of from the plan it wrote.
- A director's pass reviews the assembled episode against the original brief and
  proposes turn-level revisions through the existing revision-checked tools.
- The assistant never claims a listening result it did not receive from this tool.

### Slice 12.10: Draft Mode

Depends on: 12.1, the existing preview renderer and segment cache.

- Render a complete episode at draft fidelity using the fastest qualified local voice
  and a low-resolution video path, then selectively promote turns or scenes to final
  voices without re-rendering unchanged work.
- Draft artifacts are visibly labelled draft, are never exportable as a master, and
  are never registered as a final release member.

### Required Tests

- Parse representative, malformed, and very large speaker-attributed scripts,
  including unknown speakers, empty turns, unbalanced parentheticals, and mixed line
  endings; every rejection names the offending source line.
- Render a multi-character episode, edit one turn's text, and prove that only that
  turn regenerates while every other take, its checksum, and its history row survive.
- Reassign one character's voice and prove that exactly that character's takes
  invalidate and no other character's do.
- Prove per-turn beat overrides survive an unrelated script edit and that a declared
  overlap can never reorder turns or exceed the shorter turn.
- Apply a lexicon entry, reopen the project, and reproduce identical pronunciation
  metadata; prove one project's lexicon cannot affect another.
- Fit a generated cue to a target duration and prove the rendered episode ends on the
  outro's resolution within the timeline's microsecond tolerance.
- Prove a `bed` cue produces a ducking envelope sidechained to the speech track and
  that removing the cue removes the envelope.
- Round-trip a show format: create, instantiate two episodes, edit the format, and
  prove the already-published episode is unchanged.
- Produce a release package and verify every member's checksum, playability, and
  provenance, including a trailer whose source range maps back to real narration
  word timings.
- Run quality control against a deliberately corrupted take and prove the reported
  skipped and inserted words match a known fixture.
- Prove the assistant's listening tool is read-only, that it cannot mutate a project,
  and that its results reflect the rendered artifact rather than the manifest.
- Prove a draft artifact cannot be exported as a master or registered in a release.
- Complete one full episode on the RTX 4080 12 GB machine, interrupt it mid-render,
  restart the application, resume, and export without lost edits or hidden
  regeneration.

### Exit Gate

- One brief produces one complete, playable, checksum-registered episode with
  distinct consent-backed voices, believable turn timing, a fitted score, captions,
  and a release package - without the user issuing a per-primitive command.
- Every turn, beat, cue, placement, and format value is durable, revisable, and
  attributable to the revision that produced it.
- Quality control reports measured facts about the rendered episode and never
  presents a corrected or predicted result as a measurement.
- No draft or simulated artifact can reach a master or a release.

## Cross-Cutting Test Strategy

### Layer 1: Fast CI on Every Pull Request

- Python unit and contract tests without downloading model weights.
- React component and state-machine tests.
- Rust unit tests for persistence, paths, jobs, and worker supervision.
- Browser Playwright tests with deterministic IPC fixtures.
- Type checking, formatting, linting, dependency audit, migration validation, and
  installer syntax checks.

Target: under 15 minutes once caching is warm. Flaky tests are defects; retries do
not convert them into passing tests.

### Layer 2: Integration CI

- Build the production frontend and native shell.
- Run a fake engine process through the full RPC lifecycle.
- Exercise SQLite migration fixtures and crash recovery.
- Build packages and inspect resources, permissions, desktop entry, and CSP.
- Download generated artifacts into a clean directory and verify checksums.

### Layer 3: GPU Qualification

Run on the RTX 4080 12 GB machine for any inference or engine change:

- cold install and first generation;
- warm repeated generation;
- all supported precision variants;
- switch engines repeatedly;
- cancellation and worker kill;
- VRAM pressure and OOM recovery;
- 30-minute stability suite;
- offline generation after all network interfaces are blocked;
- fixed smoke corpus with stored metrics and output hashes where deterministic.

Results should be stored as machine-readable artifacts and linked from the release
candidate. A future self-hosted runner must be isolated from untrusted pull-request
code and must not hold the release signing key.

### Layer 4: Packaged User Journey

Test both Debian and AppImage artifacts in a clean user profile:

1. Install or launch.
2. Complete runtime setup.
3. Install the smoke model.
4. Generate and play audio.
5. Close and reopen; replay History.
6. Create a voice profile when that milestone is enabled.
7. Simulate offline use.
8. Upgrade from the previous stable version.
9. Verify user models, references, exports, and database remain intact.
10. Uninstall and confirm user data is preserved unless explicitly purged.

### Layer 5: Human Quality Review

- Use a versioned, consent-safe corpus covering narration, dialogue, numbers,
  acronyms, difficult names, punctuation, questions, emotion, and long-form text.
- Conduct blind A/B review when changing a model, vocoder, segmentation strategy,
  or default setting.
- Require multiple ratings and retain disagreement; do not overwrite subjective
  results with one aggregate score.
- Block a default-model change when objective regressions or listening preference
  cross the agreed threshold.

## Release Quality Gates

Every stable release must pass:

- clean main-branch CI;
- no unresolved critical or high vulnerability affecting the shipped path, or a
  documented temporary exception with mitigation and expiry;
- all database migrations from supported versions;
- GPU qualification for changed inference paths;
- clean Debian and AppImage user journeys;
- updater manifest, signatures, checksums, and provenance verification;
- offline launch and generation with already-installed models;
- backup and restore of representative user data;
- release notes with model/code licenses and known limitations;
- artifact inspection proving that source archives, Debian packages, AppImages,
  and update bundles contain no model weights or private voice references;
- fresh-install verification that runtime setup does not fetch model weights and
  that model installation requires an explicit user action;
- no stable navigation item whose primary action is simulated or disconnected.

Release channels should be:

- **Stable:** all gates pass; suitable for ordinary users.
- **Beta:** complete vertical slice with real execution, but collecting broader
  hardware or workflow evidence.
- **Experimental:** development setting only; data migration and compatibility may
  change. Never enabled by default.

## Feature Priority Summary

| Order | Product outcome | Unlocks |
| --- | --- | --- |
| 0 | Honest product states and complete test foundation | Safe development |
| 1 | Durable projects, jobs, history, and artifacts | Recovery and long workflows |
| 2 | Isolated engine contract and GPU scheduling | Fast, safe model expansion |
| 3 | Real voice ingestion and consent | Trustworthy cloning |
| 4 | Takes, comparison, presets, and batch production | Daily production workflow |
| 5 | Real benchmarks and routing | Evidence-based model choice |
| 6 | Chatterbox, Qwen3-TTS, and CosyVoice adapters | Competitive quality and breadth |
| 7 | Project/timeline and long-form tools | Books, podcasts, production work |
| 8 | Real-time studio | Agents, streaming, virtual microphone |
| 9 | Transcription and dubbing | Localization and speech repair |
| 10 | Local API and CLI | Ecosystem and automation |
| 11 | Finishing and trust tools | Professional delivery and governance |
| 12 | Cast, score, formats, and releases | Publishable narrative episodes |

## What We Will Not Do Yet

- Add cloud inference fallback that weakens the local-first contract.
- Promise universal model compatibility from arbitrary Hugging Face repositories.
- Merge major dependency upgrades because compilation alone passes.
- Build collaboration accounts or cloud sync before the single-user local project
  model is dependable.
- Expand text-to-music beyond its bounded local beta, or add video avatars and
  sound-effects generation, before each has dedicated hardware qualification,
  listening evidence, and license review. The current ACE-Step path accepts
  direction plus optional lyrics, while the MusicGen path is instrumental-only;
  neither is a general audio-editing or melody-conditioning surface. Phase 12's
  sound-design library is user-supplied local media registered under the existing
  rights path, not generated audio, and does not relax this rule.
- Train professional voice models before reference ingestion, consent, dataset
  quality, evaluation, and isolated training runtimes are complete.
- Call a feature complete because its screen exists.

## Immediate Next Release Sequence

1. **`0.3.0` - Honest Core:** Phase 0, feature-state labels, and first-party test
   infrastructure.
2. **`0.4.0` - Durable Studio:** Phase 1 persistence, real History, and recoverable
   jobs.
3. **`0.5.0` - Engine Platform:** Phase 2 isolation, manifests, supervisor, and real
   model lifecycle.
4. **`0.6.0` - Voice Lab:** Phase 3 real references, analysis, and consent-backed
   cloning profiles.
5. **`0.7.0` - Production Workbench:** Phase 4 takes, real Compare, presets, batch,
   and robust exports.
6. **`0.8.0` - Evidence:** Phase 5 benchmarks and transparent routing.
7. **`0.9.x` - Model Wave:** Phase 6 adapters, one independently qualified model
   family per patch release.
8. **`1.0.0` - Local Speech Harness:** Stable Phases 0-6, no simulated core
   workflows, successful upgrade testing, and published compatibility evidence.

Project Studio, Real-Time Studio, Dubbing, and the Developer Harness can then ship
as post-1.0 minor releases without weakening the 1.0 definition.

Phase 12 ships as a sequence of post-1.0 minor releases, one slice at a time. Cast
and dialogue is the first, because performance timing, the score, sound design,
formats, releases, quality control, and the assistant's listening pass all depend on
turn-scoped narration existing first.

## Roadmap Maintenance

- Update this file when a milestone enters development, changes scope, or passes
  its exit gate.
- Link implementation issues and qualification reports beside each milestone.
- Record deferred work explicitly rather than leaving inactive controls in the UI.
- Re-evaluate model priorities quarterly using upstream maintenance, licensing,
  measured local performance, and real user workflows.
- Never move a milestone to complete without attaching its automated, hardware,
  and packaged-install evidence.
