# Linux Video Studio implementation plan

Status: implementation baseline for soundAr after v0.5.5  
Reference release: Reelmify v1.2.2 (`3e0e7b92786b2dc392dd644ef0eefd810d9b39df`)  
Design references: `design-qa/video-studio/`

## Product contract

Video Studio is one focused, local-first production surface inside soundAr. It must preserve the v0.5.5 assistant, speech and music generation, Projects, History, queues, model runtimes, updater, signing, native window behavior, neutral light design, and manual sidebar collapse.

The primary result is always the assembled playable video. Scene clips and other intermediate artifacts remain available in the project inspector but never displace the master in the assistant, Projects, or History.

Public navigation contains Generate, Video Studio, Projects, Voices, Models, History, Compare, and Benchmarks. Transcribe and Live remain internal-only capabilities and are not restored as public destinations.

## Workflow invariants

1. A versioned timeline manifest is the source of truth for sources, rights, probes, transcript timing, candidates, reviewed selections, scenes, tracks, gaps, crops, captions, generated assets, renders, provenance, and revisions.
2. Time is stored as integer microseconds with rational frame rates. Candidate ranges remain in the original source clock. Timeline placement is derived once and reused by captions, tracking, previews, export, and publishing.
3. Presentation gaps and silence are preserved during transcription. Caption timing never runs on an independent clock.
4. Codex planning selects locally generated deterministic moment IDs. The service rejects invented, overlapping, excluded, stale, or otherwise invalid selections.
5. Reviewed candidates are protected. A broad regeneration cannot overwrite approved work without an explicit force boundary; targeted revision invalidates only affected stages and scenes.
6. Every mutating surface uses the same typed application service. Tauri commands, the assistant, and any CLI/HTTP adapter do not implement shadow workflows.
7. Long work returns durable job IDs. Jobs persist phase, checkpoint, attempt, idempotency key, resource class, progress, cancellation, and artifact links, and can recover safely after restart.
8. Link preview is metadata-only. Import accepts one URL, rejects playlists and bulk input, and requires a fresh unchecked rights assertion for the exact URL. The service persists the receipt and enforces it for every caller.
9. Artifacts are written to sibling staging paths, validated, checksummed, and atomically published. Final video playback uses scoped streaming/range URLs rather than loading the file into JavaScript memory.
10. Project writes require a lock token and optimistic revision precondition. A live owner is never displaced solely because a wall-clock timeout elapsed.

## Architecture

The Rust domain is split into focused modules below `app/src-tauri/src/`:

- `services/`: `AppServices`, typed service errors, and thin adapter entry points.
- `media/`: safe tool discovery, FFprobe/FFmpeg, yt-dlp, transcription runtime detection, process groups, progress, and cancellation.
- `video/contracts.rs`: manifest, source clock, candidates, scenes, tracks, captions, crop/layout, provenance, render profiles, revisions, and outputs.
- `video/service.rs`: all Video Studio operations.
- `video/timeline.rs`: exact mapping, quantization, gaps, and validation.
- `video/renderer.rs`: proxies, thumbnails, captions, layouts, audio mixing, previews, NVENC final export, and software fallback.
- `video/cache.rs`: canonical content-addressed keys and scene-level invalidation.
- `video/scheduler.rs`: light, medium, heavy, and exclusive admission across CPU, IO, VRAM, and NVENC resources.
- `video/publish.rs`: publish packages with MP4, manifest, checksums, captions, cover/thumbnail, copy, and provenance.

Schema migrations begin after v0.5.5 schema 32. Additive tables cover project kind/revision, project locks, rights receipts, media assets, immutable video versions, transcript versions, workflow stages/dependencies, multi-artifact links, cache entries, generic output records, assistant artifact links, and performance samples. Legacy audio tables and behavior remain intact.

The assistant exposes these service-backed tools with strict schemas and stable errors: `preview_link`, `import_link`, `analyze_video`, `plan_video`, `create_video_project`, `list_video_projects`, `get_video_project`, `render_video_preview`, `revise_video`, `export_video`, and `export_publish_package`.

## Visual specification

The accepted concept direction extends the existing v0.5.5 product instead of introducing a new visual language:

- true-white and neutral-gray palette using existing CSS tokens;
- Inter for interface text and JetBrains Mono for timecode and hashes;
- compact 31–38px controls, 5–8px radii, hairline borders, restrained shadows, and Lucide line icons;
- no marketing hero, sparkle icons, accent rails, gradients, decorative logos, giant cards, or filler metrics;
- one horizontal three-entry intake band: Import link, Upload video, Start from prompt or audio;
- at most three vertical editor regions: source/scenes, preview, contextual inspector;
- one horizontal timeline below the preview with visible Video, Captions, Voice, and Music tracks;
- final master and its Play, Download, Open project, and Publish package actions remain prominent;
- assistant progress is summarized by high-level production phases, not repetitive tool cards.

Responsive rules:

- wider than 1120px: full Studio plus docked assistant;
- 960–1120px: companion layout with a roughly 360px assistant and collapsed scene/inspector detail;
- below 960px: assistant becomes a full workspace mode and closing it restores playhead and selection;
- timeline overflow remains discoverable and keyboard operable; actionable labels are at least 11–12px and primary transport targets at least 36px.

## Delivery phases

### 1. Foundation

- Typed contracts and stable service errors.
- Schema migrations, locks, optimistic revisions, immutable manifests, generic artifacts, output records, and recovery.
- Durable workflow coordinator and resource-aware scheduler.
- Timeline, caption, cache-key, migration, lock, scheduler, and error tests.

### 2. Ingest and analysis

- Tool discovery across configured, system, user, package-manager, NVM, and XDG locations.
- Local upload and metadata-only link preview.
- Exact-URL rights receipt, single-source enforcement, yt-dlp import, redirects/provenance, FFprobe validation.
- Managed source copy, proxies, thumbnails, waveform, external-caption validation, and faster-whisper with original-clock timing.
- Cancellation, checkpointing, restart recovery, and cache reuse.

### 3. Planning and review

- Deterministic candidate catalog and local validation.
- Authenticated Codex research and structured scene planning.
- Candidate review, exclusions, approval protection, script/scene editing, and selective revision.

### 4. Rendering

- Fast proxy previews and incremental scene renders.
- Portrait reels, animated podcast layouts, title/speaker cards, waveforms, kinetic/calm captions, crop/scale, transitions, and audio mix.
- NVENC H.264 default with measured fallback and H.265/AV1 availability reporting.
- Atomic final export and complete publish package.

### 5. Product and assistant parity

- Video Studio entry, intake, analysis, review, workspace, render inspector, revision, and export states.
- Playable final artifacts in the assistant, Projects, and History.
- Master-first assistant cards, concise progress phases, durable thread/turn/tool/artifact links, and open/revise navigation.

### 6. Hardening and release

- Interaction, keyboard, focus, reduced-motion, zoom, and responsive tests.
- Real FFmpeg fixtures, imported-source reel, animated-podcast result, playback, restart, cache, cancellation, resume, and selective-revision verification.
- Machine baselines for ingest, transcription RTF, proxy, preview, final render, VRAM peak, cache hit, and end-to-end latency.
- Documentation, README visuals/setup, version bump, signed Debian/AppImage verification, installation, updater regression, push, and GitHub release.

## Acceptance criteria

The release is acceptable only when all of the following are true:

- Existing v0.5.5 audio, music, project, history, model, assistant, updater, signing, and native-window regression suites pass.
- Link preview performs no download; importing a URL without an exact rights receipt fails with a stable error; playlist/bulk input is rejected.
- Local upload and an unambiguously authorized imported source produce validated managed media, proxy, thumbnail, waveform, and timestamped transcript artifacts.
- Silent gaps survive transcription and final composition; captions remain within source and output bounds and follow the single timeline mapping.
- A short imported-source portrait reel and a short prompt/audio animated-podcast video render to playable MP4 files through real FFmpeg.
- Preview uses low-resolution proxies and final export uses the requested final profile; NVENC is used when its runtime smoke test passes and a software fallback remains available.
- Cancelling and restarting long work leaves no published partial artifact; resume/retry reuses safe checkpoints.
- Repeating an unchanged stage records a cache hit; changing one caption style, scene voice, or opening duration does not rerender unrelated scenes.
- A project lock prevents concurrent writers and stale revisions fail without losing either writer's work.
- The final master is playable and downloadable in Video Studio, Projects, History, and the originating assistant conversation after app restart.
- Every assistant video tool, Tauri command, and exposed API adapter is verified against the same service contract and produces durable project/job/output identifiers.
- Intake, candidate review, timeline, render, cancellation, resume, assistant, Projects, History, dialogs, and error states pass keyboard and accessibility checks at 1857/1440, 1366, 1024, 760, 540, and 390px.
- The packaged Debian build installs over the existing application without losing user data, launches successfully, renders and plays a video, verifies its updater/signing configuration, and passes release-boundary checks.

No mocked, disconnected, UI-only, or opaque-filesystem-path workflow satisfies these criteria.
