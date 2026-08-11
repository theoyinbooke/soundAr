# soundAr V1 Build Checklist

## Purpose

This is the operational build checklist for getting soundAr to a true v1 release.

It translates the higher-level v1 brief in `soundAr.md` into a practical execution tracker with:

- files to create
- implementation tasks
- tests to pass
- definition of done for each milestone

This checklist assumes the following hard constraints remain in force:

- all inference is local after model download
- no cloud inference or hosted API fallback
- support is curated, not generic
- `NeMo`, `Coqui`, and `Kokoro` are first-class engine families

## Global V1 Gates

Before calling v1 complete, all of these must be true:

- The app launches cleanly on Ubuntu Linux.
- Downloaded models remain usable when the machine is offline.
- At least one curated model works for each first-class engine family:
  `Transformers STT`, `NeMo STT`, `Transformers TTS`, `Coqui TTS`, `Kokoro TTS`.
- STT works on local audio files.
- TTS works from local text input.
- Comparison works for supported model pairs in the same task family.
- Benchmark output is locally generated and reproducible.
- Realtime transcription does not freeze microphone capture during inference.

## Canonical Milestone 1 Model Set

These are the required starter models for the first shipping milestone:

| Model ID | Task | Engine |
|---|---|---|
| `openai/whisper-tiny` | STT | TransformersSTT |
| `nvidia/parakeet-tdt-1.1b` | STT | NeMoSTT |
| `microsoft/speecht5_tts` | TTS | TransformersTTS |
| `coqui/XTTS-v2` | TTS | CoquiTTSEngine |
| `hexgrad/Kokoro-82M` | TTS | KokoroTTSEngine |

---

## Milestone 1A: Foundations

### Outcome

The application launches, the local state layout exists, and the curated model catalog is visible as app data.

### Files To Create

- `main.py`
- `requirements.txt`
- `install.sh`
- `README.md`
- `config/__init__.py`
- `config/settings.py`
- `config/constants.py`
- `core/__init__.py`
- `core/gpu_manager.py`
- `core/model_manager.py`
- `core/hub_browser.py`
- `ui/__init__.py`
- `ui/main_window.py`
- `ui/theme.py`
- `ui/widgets/__init__.py`
- `ui/widgets/model_card.py`
- `ui/widgets/download_progress.py`
- `ui/tabs/__init__.py`
- `ui/tabs/hub_tab.py`
- `ui/dialogs/__init__.py`
- `ui/dialogs/model_detail_dialog.py`
- `workers/__init__.py`
- `workers/download_worker.py`
- `data/curated_models.json`

### Tasks

- Create the repo package structure.
- Add the dependency list for PyQt6, torch, torchaudio, transformers, NeMo, Coqui TTS, audio utilities, and benchmark tooling.
- Implement app settings with local persistence under `~/.soundAr/settings.json`.
- Implement constants for app metadata, file types, and UI defaults.
- Implement a minimal GPU manager that can detect CUDA availability and basic device info.
- Implement catalog loading from `data/curated_models.json`.
- Implement model manager skeleton with local registry path under `~/.soundAr/state/models.json`.
- Implement hub browser logic that exposes curated models only.
- Implement a minimal main window and hub tab shell.
- Implement the download worker interface and progress signal structure.
- Ensure local directories are created:
  `~/.soundAr/models` and `~/.soundAr/state`.

### Tests To Pass

- App launch smoke test:
  `python3 main.py`
- Catalog load test:
  app can parse `data/curated_models.json` without error
- State directory test:
  startup creates local state directories when missing
- Download registry test:
  model manager can initialize an empty registry without crashing

### Definition Of Done

- The app launches into a visible main window.
- The curated catalog is loaded successfully.
- The hub tab can render curated model entries from local data.
- Local state paths are created automatically.
- No cloud-dependent behavior exists in the launch path.

---

## Milestone 1B: Offline Audio Core

### Outcome

Local audio files can be loaded, inspected, displayed, played, and preprocessed.

### Files To Create

- `core/audio_utils.py`
- `core/vad.py`
- `ui/widgets/audio_waveform.py`
- `ui/widgets/audio_player.py`
- `data/sample_audio/` test fixtures if needed

### Tasks

- Implement audio loading and resampling to a target sample rate.
- Implement audio info inspection without loading full files when possible.
- Implement audio save/export helpers.
- Implement waveform rendering for static audio.
- Implement local audio playback controls.
- Implement VAD wrapper with speech region extraction.
- Support the core formats required by v1:
  `wav`, `mp3`, `flac`, `ogg`, `m4a`, `webm`.

### Tests To Pass

- Load a local `.wav` file into memory successfully.
- Resample audio to `16000 Hz`.
- Render waveform from a local test audio fixture.
- Play local audio and update playback position.
- Run VAD on a speech sample and return at least one valid segment.

### Definition Of Done

- A local audio file can be loaded into the app.
- The waveform widget displays the loaded audio.
- Audio playback works with play, pause, stop, and seek.
- VAD returns usable speech segments for STT preprocessing.

---

## Milestone 1C: First-Class STT Engines

### Outcome

The app can run offline transcription using both curated STT engine families.

### Files To Create

- `engines/__init__.py`
- `engines/base_stt.py`
- `engines/stt/__init__.py`
- `engines/stt/transformers_stt.py`
- `engines/stt/nemo_stt.py`
- `workers/transcription_worker.py`
- `core/benchmark.py`
- `ui/widgets/transcript_viewer.py`
- `ui/tabs/stt_tab.py`

### Tasks

- Define the base STT engine contract.
- Implement `TransformersSTT` for `openai/whisper-tiny`.
- Implement `NeMoSTT` for `nvidia/parakeet-tdt-1.1b`.
- Implement engine load and unload behavior.
- Implement local transcription worker threading.
- Implement transcript display, copy, and export.
- Implement STT tab with model selector, audio input, VAD option, and transcription action.
- Implement STT benchmark timing and VRAM metrics.

### Tests To Pass

- `openai/whisper-tiny` downloads and loads locally.
- `openai/whisper-tiny` transcribes a local speech file.
- `nvidia/parakeet-tdt-1.1b` downloads and loads locally.
- `nvidia/parakeet-tdt-1.1b` transcribes a local speech file.
- STT benchmark returns timing and VRAM fields.
- Transcript export writes a local `.txt` file successfully.

### Definition Of Done

- Both required STT starter models work end to end.
- STT tab supports local file transcription.
- Transcript output is visible, copyable, and exportable.
- Benchmark metrics are captured for STT runs.

---

## Milestone 1D: First-Class TTS Engines

### Outcome

The app can run offline synthesis using all three curated TTS engine families.

### Files To Create

- `engines/base_tts.py`
- `engines/tts/__init__.py`
- `engines/tts/transformers_tts.py`
- `engines/tts/coqui_tts.py`
- `engines/tts/kokoro_tts.py`
- `workers/synthesis_worker.py`
- `ui/tabs/tts_tab.py`

### Tasks

- Define the base TTS engine contract.
- Implement `TransformersTTS` for `microsoft/speecht5_tts`.
- Implement `CoquiTTSEngine` for `coqui/XTTS-v2`.
- Implement `KokoroTTSEngine` for `hexgrad/Kokoro-82M`.
- Finalize the Kokoro runtime dependency and local initialization path.
- Implement TTS model load and unload behavior.
- Implement synthesis worker threading.
- Implement TTS tab with text input, model selection, playback, and save/export.
- Implement TTS benchmark timing and waveform duration metrics.

### Tests To Pass

- `microsoft/speecht5_tts` synthesizes a short local text prompt.
- `coqui/XTTS-v2` synthesizes a short local text prompt.
- `hexgrad/Kokoro-82M` synthesizes a short local text prompt.
- Generated audio can be played in-app.
- Generated audio can be saved as a local file.
- TTS benchmark returns timing, duration, and VRAM fields.

### Definition Of Done

- All three required TTS starter models work end to end.
- The TTS tab can synthesize and play output locally.
- Output audio can be saved locally.
- No TTS path requires cloud access after assets are downloaded.

---

## Milestone 1E: Comparison And Offline Reliability

### Outcome

The app can compare supported local models side by side and remains usable offline once models are present.

### Files To Create

- `workers/benchmark_worker.py`
- `ui/tabs/compare_tab.py`
- `ui/dialogs/export_dialog.py`
- `ui/tabs/settings_tab.py`

### Tasks

- Implement comparison workflow for STT model pairs.
- Implement comparison workflow for TTS model pairs.
- Implement benchmark result export.
- Implement model-management view in settings.
- Implement error handling for:
  missing model files, corrupt downloads, OOM, unsupported local state, and download interruption.
- Verify the app behavior when network access is unavailable after download.

### Tests To Pass

- Compare two local STT models on the same audio input.
- Compare two local TTS models on the same text input.
- Export comparison results to local JSON or CSV.
- Remove network access and confirm downloaded models still load and infer.
- Delete a model and confirm the local registry updates correctly.

### Definition Of Done

- Comparison works for supported model pairs.
- Results export is functional.
- Downloaded models remain usable while offline.
- Model management actions behave correctly.

---

## Milestone 1F: Realtime

### Outcome

Live microphone transcription works without blocking capture while inference is in progress.

### Files To Create

- `workers/realtime_worker.py`
- `ui/tabs/realtime_tab.py`

### Files To Update

- `ui/widgets/audio_waveform.py`
- `core/vad.py`
- `engines/base_stt.py` if realtime-specific hooks are needed

### Tasks

- Implement microphone capture using `sounddevice`.
- Separate audio capture/VAD from transcription execution.
- Add a bounded queue for finished speech segments.
- Implement realtime waveform updates.
- Implement VAD-active state in the UI.
- Append transcript chunks incrementally.
- Handle stop/start transitions cleanly.

### Tests To Pass

- Detect available input devices.
- Start and stop microphone capture without crashing.
- Show live waveform during recording.
- Produce transcript chunks from speech input.
- Keep capture responsive even when transcription latency increases.

### Definition Of Done

- Realtime transcription is usable on the target machine.
- Capture does not freeze while the GPU is busy.
- Transcript chunks appear incrementally.
- Stop/start behavior is stable and does not leak threads or streams.

---

## Release Hardening

### Tasks

- Add keyboard shortcuts for core actions.
- Save and restore window state and last-used settings.
- Add Ubuntu `.desktop` integration.
- Improve error messages and recovery guidance.
- Verify startup behavior on clean and partially populated local state.
- Review product copy so it never implies unsupported generic HuggingFace compatibility.

### Tests To Pass

- Full clean-machine setup walkthrough
- Full offline walkthrough after starter models are downloaded
- App restart preserves expected user state
- Basic regression pass across hub, STT, TTS, compare, settings, and realtime

### Definition Of Done

- A new user can install, download, and use the app without hidden manual steps.
- The app behaves predictably across restarts.
- All v1 acceptance criteria are satisfied.

---

## Suggested Test Matrix

### Smoke Tests

- app launch
- catalog load
- local state creation
- one model download
- one STT transcription
- one TTS synthesis

### Hardware-Gated Tests

- CUDA detection
- VRAM reporting
- Whisper transcription on GPU
- Parakeet transcription on GPU
- XTTS synthesis on GPU if required by runtime

### Network-Gated Tests

- catalog metadata refresh
- model download and resume

### Offline Tests

- launch app without network
- load already-downloaded models
- run STT and TTS without network
- compare local models without network

### Manual Tests

- microphone device selection
- realtime transcription responsiveness
- audio playback UX
- export dialogs

---

## Ship Checklist

- Milestone 1A complete
- Milestone 1B complete
- Milestone 1C complete
- Milestone 1D complete
- Milestone 1E complete
- Milestone 1F complete
- Release hardening complete
- All five starter models verified
- Offline workflow verified
- No cloud-dependent code paths remain in v1 runtime
