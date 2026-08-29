# Changelog

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
