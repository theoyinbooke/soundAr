# Linux Video Studio implementation plan

Status: implementation baseline for soundAr after v0.5.5  
Reference release: Reelmify v1.2.2 (`3e0e7b92786b2dc392dd644ef0eefd810d9b39df`)  
Design references: `design-qa/video-studio/`

## Product contract

Video Studio is one focused, local-first production surface inside soundAr. It must preserve the v0.5.5 assistant, speech and music generation, Projects, History, queues, model runtimes, updater, signing, native window behavior, neutral light design, and manual sidebar collapse.

The product is a general composition engine, not only a video importer. Source video, soundAr speech and music, local or agent-generated still images, illustrations, image sequences, waveform/title/speaker elements, captions, and locally rendered intermediates are all first-class, timed assets on the same canonical project timeline. Still images may be animated with deterministic pan, zoom, crop, opacity, and transitions without requiring a conventional text-to-video model.

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
11. Visual assets retain their exact managed-file checksum, dimensions, provenance, generation prompt/model/seed when available, and the durable job/tool invocation that produced them. A later retry adopts the exact asset or fails closed; it never silently replaces published bytes.
12. UI, authenticated Codex tools, and headless CLI/MCP/HTTP adapters submit identical edit and render requests to the same services. Headless mode starts before any Tauri window bootstrap, so an agent can create, monitor, revise, and export a project while the desktop app is closed.
13. Timeline edits are real manifest mutations: split, trim, reorder, merge, caption geometry, and visual-layer transforms use project locks, version CAS, idempotent operation IDs, exact source-clock math, and selective invalidation. Pointer movement may preview locally, but persistence occurs once through the shared service and rolls back visibly on failure.

## Architecture

The Rust domain is split into focused modules below `app/src-tauri/src/`:

- `video/contracts.rs`: manifest, source clock, candidates, scenes, tracks, captions, crop/layout, narration bindings, provenance, render profiles, revisions, and outputs.
- `video/media.rs`: bounded tool discovery and execution, FFprobe/FFmpeg and yt-dlp policy, public-only HTTPS confinement, local-media validation, caption validation, process groups, progress, and cancellation.
- `video/timeline.rs`: exact mapping, quantization, half-open sample ranges, editable endpoints, gaps, and validation.
- `video/editor.rs`: pure deterministic split, trim, reorder, merge, and edit receipts over the canonical manifest; service integration owns locking, idempotency, and version CAS.
- `video/visuals.rs`: still-image and illustration assets, image sequences, normalized transforms, deterministic motion paths, transitions, provenance, validation, and FFmpeg composition inputs.
- `video/assembly.rs`: canonical multi-scene audio/video graph construction from the reviewed timeline manifest.
- `video/renderer.rs`: proxies, thumbnails, waveforms, portrait layouts, FFmpeg progress, validation, and atomic file publication.
- `video/cache.rs`: canonical content-addressed keys and scene-level invalidation.
- `video/scheduler.rs`: light, medium, heavy, and exclusive admission across CPU, IO, VRAM, and NVENC resources.
- `video/service.rs`: durable ingest, analysis artifacts, transcription, narration replacement, preview/final rendering, variations, publishing, recovery, and shared application contracts.
- `video/presentation.rs`: one safe UI/assistant projection for projects, jobs, and playable artifacts.
- `video_commands.rs`: thin Tauri and assistant-facing orchestration over the same service, including Codex planning, existing soundAr synthesis, timeline edits, and visual-asset composition.
- `store.rs`: additive schema migrations, locks, jobs, immutable versions, rights receipts, exact artifact/output publication, assistant links, and crash recovery.
- `agent_cli.rs`: headless entry point for the same typed operations. The packaged soundAr binary dispatches `agent` before Tauri initialization and may expose thin CLI, stdio MCP, or authenticated loopback HTTP adapters without business logic.

Schema migrations begin after v0.5.5 schema 32 and advance additively through schema 37. The new tables cover project kind/revision, project locks, rights receipts, media assets, immutable video versions, transcript versions, workflow stages/dependencies, multi-artifact links, cache entries, generic output records, assistant artifact links, and performance samples. Legacy audio tables and behavior remain intact.

The assistant exposes these service-backed tools with strict schemas and stable errors: `preview_link`, `import_link`, `analyze_video`, `plan_video`, `create_video_project`, `list_video_projects`, `get_video_project`, `edit_video_timeline`, `add_visual_asset`, `render_video_preview`, `revise_video`, `export_video`, and `export_publish_package`. `add_visual_asset` registers and places one user-selected or locally generated PNG/JPEG/WebP on the canonical clock in one durable operation, including its motion, fit, crop, transition, and provenance. An image generator is optional and provider-independent: when Codex or another local tool produces an image, soundAr validates and registers the resulting file rather than embedding a cloud-provider workflow.

## Visual specification

The accepted concept direction extends the existing v0.5.5 product instead of introducing a new visual language:

- true-white and neutral-gray palette using existing CSS tokens;
- Inter for interface text and JetBrains Mono for timecode and hashes;
- compact 31–38px controls, 5–8px radii, hairline borders, restrained shadows, and Lucide line icons;
- no marketing hero, sparkle icons, accent rails, gradients, decorative logos, giant cards, or filler metrics;
- one horizontal three-entry intake band: Import link, Upload video, Start from prompt or audio;
- at most three vertical editor regions: source/scenes, preview, contextual inspector;
- one horizontal timeline below the preview with visible Video, Captions, Voice, and Music tracks;
- timeline density has three explicit states: collapsed restores the canvas, compact shows all core lanes without noise, and expanded exposes trim/split/reorder handles and fine timing;
- scene rows and ordinary timeline blocks show concise titles only; timing and provenance remain available to assistive labels, tooltips, and the inspector;
- captions and supported visual elements are directly selectable, draggable, and resizable on the preview, with the same normalized geometry consumed by preview and final rendering;
- the final master is centered, aspect-correct, and owns the export canvas; Download, Open project/folder, and Publish package live in the compact top-right toolbar while completion is a transient toast plus status badge;
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
- Headless `soundar agent` commands plus matching MCP/HTTP payloads for create, get/list, ingest, plan, edit, preview, export, cancel, resume, and monitor operations while the GUI is not running.

### 6. Generated visual composition

- Managed PNG/JPEG/WebP still and illustration import with bounded decoding, exact checksums, dimensions, alpha/color metadata, provenance, and atomic publication.
- Timed visual layers and image sequences on canonical tracks, including fit/fill, crop, normalized bounds, z-order, opacity, rotation, deterministic pan/zoom motion, and bounded transitions.
- FFmpeg preview/final parity for visual transforms, content-addressed scene caches, selective rerendering, and a software path that does not require an image-generation model.
- Agent workflow that can plan shot prompts, register images produced by available Codex/local tools, assemble them with soundAr speech/music/captions, monitor durable jobs, and revise only affected visual scenes.

### 7. Hardening and release

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
- With the desktop app closed, the packaged headless CLI can create or reopen a project, register a rights-safe generated image, add and animate it on the timeline, render a preview, resume after interruption, export a final master, and leave that master visible when the GUI next opens.
- A short image-driven project made from at least three still/illustration assets, real soundAr speech, real soundAr music, and source-clock captions renders as a playable portrait MP4; changing one image transform reuses every unaffected scene and audio artifact.
- Scene rows and timeline clips are title-only at rest; all four core lanes fit in compact mode; collapsing the timeline measurably increases preview area; expanded mode exposes keyboard-operable trim/split/reorder controls.
- Dragging or resizing a caption persists normalized canvas geometry and changes preview/final placement without changing any caption page, word, source-clock, or voice timing boundary.
- Intake, candidate review, timeline, render, cancellation, resume, assistant, Projects, History, dialogs, and error states pass keyboard and accessibility checks at 1857/1440, 1366, 1024, 760, 540, and 390px.
- The packaged Debian build installs over the existing application without losing user data, launches successfully, renders and plays a video, verifies its updater/signing configuration, and passes release-boundary checks.

No mocked, disconnected, UI-only, or opaque-filesystem-path workflow satisfies these criteria.
