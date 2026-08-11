# soundAr — V1 Implementation Brief

## Product Intent

soundAr is a Linux desktop workbench for running open-source speech models locally. The app lets a user browse a curated set of supported STT and TTS models, download them from HuggingFace, and then use them fully offline for transcription, synthesis, comparison, and benchmarking.

This document is the execution brief for v1. It is intentionally narrower than the full architecture plan and is meant to guide implementation order and scope control.

## Hard Constraints

- All inference must run locally after model download.
- No cloud inference, hosted APIs, API keys, telemetry-dependent features, or online fallbacks are allowed.
- The app may use HuggingFace only for curated metadata refresh and model download.
- V1 must optimize for Ubuntu Linux on an NVIDIA RTX 4080 with 12GB VRAM.
- `NeMo`, `Coqui`, and `Kokoro` are first-class engine families, not future placeholders.
- Model support is curated. We do not promise generic compatibility with arbitrary HuggingFace repos.

## V1 Definition

V1 ships when the app can do all of the following reliably:

- Browse a curated model catalog from inside the app.
- Download supported models and track them locally.
- Run offline STT on local audio files.
- Run offline TTS from local text input.
- Support at least one working curated model for each first-class engine family:
  `Transformers STT`, `NeMo STT`, `Transformers TTS`, `Coqui TTS`, and `Kokoro TTS`.
- Perform local side-by-side comparison between two supported models in the same task family.
- Run local benchmarking with clear definitions for timing and VRAM reporting.
- Continue working with already-downloaded models while fully offline.

## Explicit Non-Goals For V1

- Cloud API testing of any kind.
- Generic “paste any HuggingFace model ID” support.
- Cross-platform packaging beyond Ubuntu-first development.
- Fine-tuning, training, diarization, subtitle generation, or batch processing.
- Fancy always-on GPU dashboards or title-bar VRAM widgets.

## Product Shape

The app should expose three model tiers in the curated catalog:

- `smoke`: small models used for fast testing and CI-friendly validation.
- `recommended`: the default models we expect most users to try first on the target machine.
- `advanced`: heavier or more specialized models that are supported, but not the default first-run path.

This keeps the product honest: first-class engine support does not require every model in that engine family to be equally prioritized on day one.

## Curated Starter Catalog

These are the exact starter models for the first implementation milestone.

### Required For Milestone 1

| Model ID | Task | Engine | Tier | Why It Is In M1 |
|---|---|---|---|---|
| `openai/whisper-tiny` | STT | TransformersSTT | smoke | Fast download and minimal-cost transcription path for first end-to-end validation |
| `nvidia/parakeet-tdt-1.1b` | STT | NeMoSTT | recommended | Validates first-class NeMo support with a model aimed at speed |
| `microsoft/speecht5_tts` | TTS | TransformersTTS | smoke | Small, well-known TTS path for baseline synthesis and UI integration |
| `coqui/XTTS-v2` | TTS | CoquiTTSEngine | recommended | Validates first-class Coqui support and multilingual/voice-cloning-capable runtime |
| `hexgrad/Kokoro-82M` | TTS | KokoroTTSEngine | recommended | Validates first-class lightweight TTS support with a low-latency option |

### Add Immediately After Milestone 1 Stabilizes

| Model ID | Task | Engine | Tier | Why It Comes Next |
|---|---|---|---|---|
| `openai/whisper-large-v3-turbo` | STT | TransformersSTT | recommended | Better default STT quality/speed balance on the target GPU |
| `distil-whisper/distil-large-v3` | STT | TransformersSTT | recommended | Strong English-focused fast path |
| `nvidia/canary-1b` | STT | NeMoSTT | advanced | Expands first-class NeMo support beyond the speed-first model |
| `facebook/wav2vec2-large-960h` | STT | TransformersSTT | advanced | Useful CTC path and engine coverage expansion |
| `facebook/mms-tts-eng` | TTS | TransformersTTS | advanced | Adds language-family coverage on the Transformers TTS side |
| `suno/bark` | TTS | TransformersTTS | advanced | Expressive output, but heavier and more specialized |
| `nari-labs/Dia-1.6B-0626` | TTS | TransformersTTS | advanced | Multi-speaker dialogue path after the core TTS flows are stable |
| `sesame/csm-1b` | TTS | TransformersTTS | advanced | More specialized conversational path after the baseline TTS engine is proven |

## Model Catalog Rules

`data/curated_models.json` is the source of truth for supported models in v1.

Each entry should declare:

- `model_id`
- `task`
- `engine`
- `tier`
- `recommended_for_12gb`
- `languages`
- `summary`
- `known_limitations`
- `test_status`
- `default_chunk_seconds` for STT when relevant
- `default_sample_rate`
- `runtime_notes`

Suggested shape:

```json
{
  "version": 1,
  "models": [
    {
      "model_id": "openai/whisper-tiny",
      "task": "stt",
      "engine": "transformers",
      "tier": "smoke",
      "recommended_for_12gb": true,
      "languages": ["multilingual"],
      "summary": "Small Whisper model for fast end-to-end validation.",
      "known_limitations": ["Lower accuracy than larger Whisper variants"],
      "test_status": "required",
      "default_chunk_seconds": 30,
      "default_sample_rate": 16000,
      "runtime_notes": ["Use as the default STT smoke-test model"]
    }
  ]
}
```

## Architecture Decisions That Should Stay Fixed

### 1. Engine Resolution Is Catalog-Driven

The app should resolve model engine from the curated catalog, not from tag guessing or config heuristics. Unknown model IDs should be treated as unsupported in v1.

### 2. Offline Runtime State Is User-Local

Downloaded models live under `~/.soundAr/models`. Mutable app state lives under `~/.soundAr/state`, including:

- `models.json`
- `benchmark_history.json`
- any future local cache or catalog snapshots

### 3. Realtime Must Use Separate Capture And Inference Stages

Microphone capture must not block on GPU inference. Realtime should use:

- one queue for audio capture/VAD ingestion
- one bounded queue for completed speech segments awaiting transcription
- clear backpressure behavior when inference falls behind

### 4. Benchmark Reporting Must Be Honest

Benchmark results should distinguish:

- model load time
- warmup time
- inference-only time
- model-side peak allocation
- user-facing total VRAM snapshot when available

## Implementation Order

### Milestone 1A: Foundations

- Project scaffolding
- settings/config/constants
- theme and main window shell
- local state directories
- curated catalog file
- model manager
- hub browser UI for curated models only

Exit criteria:

- The app launches.
- The curated catalog is visible in the UI.
- A supported model can be downloaded and appears in local state.

### Milestone 1B: Offline Audio Core

- audio loading/saving/resampling
- waveform widget
- audio player
- VAD wrapper

Exit criteria:

- A local audio file can be loaded, displayed, played back, and preprocessed.

### Milestone 1C: First-Class STT Engines

- `TransformersSTT`
- `NeMoSTT`
- transcription worker
- STT tab
- benchmark core for STT

Exit criteria:

- `openai/whisper-tiny` transcribes local audio.
- `nvidia/parakeet-tdt-1.1b` transcribes local audio.
- Results can be copied/exported locally.

### Milestone 1D: First-Class TTS Engines

- `TransformersTTS`
- `CoquiTTSEngine`
- `KokoroTTSEngine`
- synthesis worker
- TTS tab
- benchmark core for TTS

Exit criteria:

- `microsoft/speecht5_tts` synthesizes audio locally.
- `coqui/XTTS-v2` synthesizes audio locally.
- `hexgrad/Kokoro-82M` synthesizes audio locally.
- Output audio can be played and saved locally.

### Milestone 1E: Comparison And Offline Reliability

- compare tab
- comparison report export
- offline mode verification
- error handling for missing models, corrupt models, and OOM

Exit criteria:

- Two local models in the same task family can be compared side by side.
- The app remains usable with network access disabled, assuming models are already downloaded.

### Milestone 1F: Realtime

- realtime worker with separate capture/inference stages
- realtime tab
- bounded-latency behavior

Exit criteria:

- Live microphone transcription works without freezing capture while inference is busy.

## Default User Experience

The first-run path should be simple:

1. Open the app.
2. See a curated catalog, not a raw model marketplace.
3. Download one recommended STT model and one recommended TTS model.
4. Run an audio-file transcription.
5. Generate speech from a short text prompt.
6. See clear local metrics without needing any online service.

## Acceptance Criteria

V1 is ready when all of the following are true:

- The app runs end-to-end on Ubuntu with the target GPU.
- Every first-class engine family has at least one working curated model path.
- No part of inference depends on internet access after download.
- The compare flow works for both STT and TTS where the curated pair is supported.
- Benchmark exports are locally generated and reproducible.
- The product language never implies unsupported generic model compatibility.

## Open Decisions To Resolve Early

- Exact Kokoro runtime dependency and version pin
- Whether `coqui/XTTS-v2` is in the first public milestone or enabled behind an “advanced” flag until stable
- Which models should be bundled as demo recommendations versus simply supported in the catalog
- Whether `openai/whisper-large-v3-turbo` becomes the default recommended STT model immediately or only after smoke-test stability is proven

## Recommended Next Action

Start with `Milestone 1A` and create the first draft of `data/curated_models.json` using only the five required `Milestone 1` starter models. That gives the implementation a tight contract from day one and prevents the UI and engine layer from drifting into generic unsupported-model behavior.
