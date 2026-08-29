# soundAr Release Checklist

Complete this checklist against the exact candidate artifacts. Record the commit,
version, package SHA-256 values, UTC time, machine, GPU, driver, and operator with
the retained logs. A skipped required item blocks publication.

## Source And Automation

- [ ] Main-branch CI is green for the candidate commit.
- [ ] Version metadata and tag agree: `./scripts/check-release-version.sh <tag>`.
- [ ] Production dependencies have no unresolved shipped-path high or critical issue.
- [ ] Production boundary passes after a clean build: `npm run build --prefix app && ./scripts/verify-production-boundary.sh && npm run test:production --prefix app`.
- [ ] Python 3.11 contracts, React tests, full Playwright matrix, Rust tests, formatting, and script syntax pass as listed in `docs/test-matrix.md`.
- [ ] A real exported master decodes in the shipping webview: `npm run test:playback --prefix app`.
- [ ] Release notes distinguish functional, beta, experimental, disabled, and known limitations.

## Packages And Updates

- [ ] CI produced signed Debian and AppImage artifacts from the candidate commit.
- [ ] `./scripts/verify-linux-bundles.sh <version>` passes in signed mode.
- [ ] SHA-256 checksums, updater signatures, updater JSON, and provenance point to the same bytes.
- [ ] A clean downloaded copy verifies against published checksums.
- [ ] AppImage update downloads, verifies, installs, restarts, and reports the new version.
- [ ] Debian update opens the correct release and the package upgrade succeeds through the system package manager.
- [ ] The package contains no credentials, voice references, generated user audio, or model weights.

## Clean User Journey

- [ ] Debian installs on a clean supported Linux profile and launches from the desktop entry.
- [ ] AppImage launches on a clean supported Linux profile.
- [ ] First-time runtime setup succeeds without downloading any model weights.
- [ ] A model download occurs only after explicit confirmation of source, revision, license, size, access, and hardware requirements.
- [ ] The smoke model generates decodable audio; Play, seek, export, History replay, close/reopen, and delete work.
- [ ] Voice import, consent evidence, reference processing, preview, edit, and delete work when Voice Lab changed.
- [ ] Offline relaunch and generation work with the installed runtime/model while GitHub and Hugging Face are unreachable.
- [x] Previous-release upgrade preserves database, settings, projects, jobs, History, exports, references, registry, and model files.
- [ ] Debian uninstall preserves user data; explicit purge removes only documented managed data after confirmation.

## GPU And Stability

- [x] Packaged GPU acceptance passes against the candidate AppImage resources on the RTX 4080 12 GB machine.
- [x] Cold and warm generations, parallel jobs, rolling batch, comparison, load/unload, cancellation, and worker-kill recovery pass.
- [x] `SOUNDAR_SOAK_DURATION_SECONDS=1800 ./scripts/run-packaged-gpu-soak.sh` completes against the exact candidate without leaked workers or scheduler reservations; its JSON report passes validation.
- [x] Peak VRAM, RTF, startup overhead, driver, runtime, and model revisions are retained as machine-readable evidence.
- [ ] Already-installed models complete the offline GPU smoke.
- [ ] `./scripts/run-packaged-musicgen-acceptance.sh` passes against the exact candidate, isolated MusicGen runtime, and pinned `facebook/musicgen-small` checkpoint; cold/warm 32 kHz artifacts, durable Music history, unload, and scheduler quiescence are retained.
- [x] `./scripts/run-packaged-acestep-acceptance.sh` passes against the exact candidate, isolated ACE-Step runtime, and pinned `ACE-Step/Ace-Step1.5` Studio checkpoint; consecutive cold/warm 48 kHz stereo lyric renders, separate prompt/lyrics History fields, unload, and scheduler quiescence are retained.

## Video Studio

- [ ] `scripts/video/test-harness.sh` passes with real FFmpeg/FFprobe fixture generation, proxy cache reuse, preview/final decode, atomic publication, and regression-gate self-test.
- [ ] The full-resolution benchmark passes on the release machine with CUDA transcription configured; every stage is measured, no stage is skipped, and `benchmark.json` plus its SHA-256 are retained.
- [ ] `scripts/video/qualify_gpu_overlap.py` passes three consecutive repetitions for any enabled inference/render overlap; every unqualified model or workload remains serialized.
- [ ] One rights-clear local imported-source reel and one prompt/audio animated podcast render to playable final MP4 masters and publish packages through production services.
- [ ] Exact-URL rights confirmation, local-media validation, public-network confinement, source/output quotas, private storage, and publish-package redaction/integrity tests pass.
- [ ] Source-clock transcript gaps, candidate review protection, captions, scene/timeline math, audio mix, crop/layout, cancellation, resume after restart, cache reuse, and revision without full rerender are verified.
- [ ] “Change the voice” creates one durable idempotent speech replacement and invalidates only speech/render/package work; “render three variations” publishes one primary master plus variants from one frozen editorial version.
- [ ] Video Studio, Projects, History, and the Assistant all play and download the registered assembled master; scene artifacts remain secondary and no result is only an opaque local path.
- [ ] Responsive and accessibility checks pass with the Assistant open and closed at 800, 1024, 1180, and 1440 px, with no unintended overflow or automatic sidebar collapse.
- [ ] Packaged Debian/AppImage runtime discovery reports the same managed FFmpeg, yt-dlp/EJS/Node, and faster-whisper readiness as the verified source candidate.

## Playback And Quality

- [ ] Real output selection and routed playback complete without persistent underruns.
- [ ] Output hot-unplug/reconnect recovers with a truthful visible state.
- [ ] Consent-safe listening corpus is reviewed for clarity, pronunciation, numbers, names, pacing, and long-form continuity.
- [ ] New or changed cloning models have retained consent, human preference, and comparative similarity evidence.
- [ ] MusicGen has a retained listening review covering ambient, rhythmic, acoustic, electronic, and negative-vocal prompts; unacceptable artifacts, prompt limits, and the CC BY-NC 4.0 non-commercial restriction are disclosed.
- [ ] ACE-Step has a retained listening review covering Song and Instrumental modes, sectioned lyrics, language selection, intelligibility, music-direction adherence, 48 kHz stereo delivery, and source-audio consent boundaries.
- [ ] `MODEL_LICENSES.md` and in-app disclosures match every qualified model and pinned revision.

## Publication And Rollback

- [ ] Database backup from the candidate opens and restores on a copied profile.
- [ ] The prior stable package and rollback instructions are available.
- [ ] Draft assets install and run before the release is published.
- [ ] Published updater metadata resolves only after every required gate passes.
- [ ] Post-publication update detection is confirmed from the prior stable AppImage.
