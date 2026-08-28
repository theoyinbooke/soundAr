# Video Studio performance baseline

This document defines the reproducible Linux media gate for Video Studio. It measures the real FFmpeg/FFprobe/NVENC path with locally generated, unambiguously rights-clear inputs. It does not use a cloud service, download a model, access a source URL, or mutate system packages.

## Reproduce the gate

```bash
scripts/video/check-toolchain.sh --nvenc-smoke
scripts/video/test-harness.sh
scripts/video/run-smoke-benchmark.sh --output-dir evidence/video-studio-performance
```

Every invocation creates a new immutable run directory. Existing fixtures, outputs, and reports are never overwritten. `benchmark.json` contains:

- OS, kernel, CPU, logical cores, RAM, output-volume capacity, GPU, driver, VRAM, compute capability, and Git commit;
- resolved executable paths, versions, hardware accelerators, filters, encoders, and NVENC runtime smoke result;
- fixture rights receipt, timing contract, manifest checksum, and source hashes;
- wall time, input duration, real-time factor (RTF), command, intended encoder, output codec, size, dimensions, checksum, and decode validation per stage;
- baseline and peak GPU VRAM, peak GPU utilization, and peak encoder utilization per measured command;
- a content-addressed proxy miss and validated cache hit with identical output hashes;
- optional faster-whisper RTF when an existing local model directory is supplied;
- regression-gate results from `scripts/video/performance-thresholds.json`.

RTF is wall time divided by source duration. Values below 1.0 are faster than real time. Absolute GPU memory includes other desktop processes; scheduling decisions should use both the recorded baseline and peak delta.

## Exact-machine baseline

Release-qualified reference run: `20260828T051916Z-1130289` on 2026-08-28 UTC at benchmark commit `d74a266`. The immutable report SHA-256 is `db2bc22978f84a145c253506bd7c0fbfa19b27c32c932dfa92d9da0e4d5eb2a7`.

| Component | Baseline |
|---|---|
| OS / kernel | Ubuntu 26.04 LTS / 7.0.0-30-generic |
| CPU | Intel Core i9-13900HX, 32 logical CPUs |
| RAM | 64,204,172 KiB reported by Linux |
| GPU | NVIDIA GeForce RTX 4080 Laptop GPU, compute capability 8.9 |
| Driver / VRAM | 595.84 / 12,282 MiB |
| FFmpeg | 8.0.1-3ubuntu2 |
| Final encoder | H.264 NVENC (`h264_nvenc`, runtime-smoke verified) |
| Tool readiness | FFmpeg/FFprobe/NVENC ready; Node 22.23.2 ready; faster-whisper 1.2.1/CTranslate2 4.6.0 CUDA ready; yt-dlp 2026.6.9/yt-dlp-ejs 0.8.0 ready |

The benchmark used one 6.000 s moving imported-source fixture and one 7.320 s animated-podcast fixture. Both contain AAC audio; the imported source carries three caption cues with visible source-clock gaps. The locally synthesized podcast speech contains an explicit two-second silence. Final output is 1080×1920.

| Stage | Wall time | RTF | Encoder | Peak VRAM | Peak delta |
|---|---:|---:|---|---:|---:|
| Imported-source probe | 0.074 s | 0.0124 | n/a | 1,264 MiB | 0 MiB |
| Podcast-source probe | 0.072 s | 0.0098 | n/a | 1,264 MiB | 0 MiB |
| 640×360 proxy, cache miss | 0.249 s | 0.0415 | libx264 ultrafast | 1,264 MiB | 0 MiB |
| 640×360 proxy, validated cache hit | 0.135 s | 0.0225 | no render | n/a | n/a |
| 540×960 portrait preview | 0.291 s | 0.0485 | libx264 ultrafast | 1,264 MiB | 0 MiB |
| 1080×1920 imported reel final | 0.857 s | 0.1428 | H.264 NVENC | 1,603 MiB | 339 MiB |
| 1080×1920 animated podcast final | 0.906 s | 0.1238 | H.264 NVENC | 1,603 MiB | 339 MiB |
| faster-whisper transcription | 1.905 s | 0.2603 | CUDA FP16 | 1,604 MiB | 340 MiB |

End-to-end wall time was 7.817 s, including rights-clear fixture generation, all probes, proxy miss/hit, preview, two final renders, CUDA transcription, checksums, FFprobe validation, and first-frame decode checks. All twelve regression checks passed. The test cache hit ratio is intentionally 0.5 because it issues one miss followed by one hit for the same canonical key.

Output validation evidence:

| Artifact | Bytes | SHA-256 |
|---|---:|---|
| Proxy / cache-hit payload | 818,731 | `49d9a0e160f5b750b5c4889f9edf865d1914dc9bd20695bac1157841aad7c626` |
| Portrait preview | 946,455 | `20c8aa2864cc628339d5bad5aa38f832e34e39eb15ca4aa3d65c229442ce7176` |
| Imported reel final | 5,189,244 | `2a54d590571ebe73b493c43dd2def6ff84310ad173a9d213f38aa890e59488ac` |
| Animated podcast final | 828,113 | `a9c92aea3f97dd2fc84d1c8e4877e4da9658c578c80bc42ca24db5aaf6fa1ec9` |
| Timestamped transcript | 3,140 | `a55909792c42d373f2d7d7ab12701576b33073c4e7b4b8bfe6f3fa0e31d8eae9` |

These hashes identify this reference run, not universal golden video bytes. FFmpeg, encoder, driver, and muxer updates can legitimately change encoded bytes. The invariant is that a cache hit within one toolchain key exactly matches its miss, and every published output probes and decodes successfully.

## Regression gate

The checked-in thresholds are deliberately wider than the observed baseline. They catch software fallbacks, broken filters, runaway scaling, cache bypasses, and obvious thermal/resource regressions without failing on ordinary desktop variance.

| Gate | Maximum |
|---|---:|
| End-to-end wall time | 15.0 s |
| Probe RTF | 0.05 each |
| Proxy cache-miss RTF | 0.15 |
| Preview RTF | 0.25 |
| Each final-render RTF | 0.65 |
| faster-whisper RTF when configured | 0.80 |
| Cache-hit wall / miss wall | 0.90 |
| Each final-render VRAM delta | 2,048 MiB |
| faster-whisper VRAM delta when configured | 4,096 MiB |

The benchmark exits nonzero and records the failed check in `benchmark.json` when any threshold is exceeded. A threshold change requires a new idle-machine baseline, a written explanation, and review; it is not an acceptable way to make a regression disappear.

For a CPU-only CI smoke:

```bash
scripts/video/run-smoke-benchmark.sh \
  --output-dir evidence/video-studio-performance \
  --encoder libx264 \
  --quick
```

`--quick` keeps all stages and validations but renders final artifacts at 540×960. Release qualification must run the default full-resolution profile with `--encoder auto` and confirm that the selected encoder matches the report.

## Transcription qualification

The release-qualified run uses the managed transformers environment with faster-whisper 1.2.1, CTranslate2 4.6.0, CUDA FP16, and a content-addressed local conversion of the already installed `openai/whisper-tiny` smoke model. It performs no model lookup or download. The model content fingerprint recorded in the transcript is `fc48c04033b8db32ec42171b709f60d84525472d9fc3fe627fd931ee63976583`.

The result contains two segments and twelve words on the original microsecond clock. With VAD explicitly disabled, it retains a measured 2,320,000 µs gap between the two synthesized phrases. The full child-process wall time is 1.905 s for 7.320 s of audio (RTF 0.2603), with a measured 340 MiB peak VRAM delta. Reproduce it without a model download:

```bash
scripts/video/run-smoke-benchmark.sh \
  --output-dir evidence/video-studio-performance \
  --transcription-model "$SOUNDAR_WHISPER_MODEL_PATH" \
  --faster-whisper-python "$SOUNDAR_FASTER_WHISPER_PYTHON"
```

The gate fails if the transcription is empty, word timing is missing, the source-clock gap is collapsed, or VAD-disabled timing is not recorded. It also records model hash, device, precision, wall time, RTF, and VRAM.

## Qualified Whisper/NVENC overlap

Release-qualified overlap run: `20260828T054049Z-1146496`. The immutable `gpu-overlap.json` SHA-256 is `2456adc1bf5c26c16db6e2f3371e73adae397b2afbb812964d5a0904e71b5026`.

The overlap gate starts a repeated 60-second 1080×1920 H.264 NVENC final render, waits until the encoder is active, then runs the exact managed `openai/whisper-tiny` CTranslate2 model on CUDA FP16. It continuously samples total VRAM, validates the rendered A/V through FFprobe and first-frame decode, and validates the timestamped transcript and its preserved source-clock gap. Three consecutive repetitions are mandatory.

| Repetition | Process overlap | Render RTF | Transcription RTF | Peak total VRAM | Peak delta |
|---:|---:|---:|---:|---:|---:|
| 1 | 2.169 s | 0.0861 | 0.2964 | 1,940 MiB | 676 MiB |
| 2 | 2.069 s | 0.0868 | 0.2826 | 1,974 MiB | 710 MiB |
| 3 | 2.019 s | 0.0854 | 0.2758 | 1,974 MiB | 710 MiB |

All repetitions stayed far below the 11,514 MiB usable scheduler budget (12,282 MiB physical minus 768 MiB headroom), retained twelve timestamped words and the intentional gap, and observed 50% peak encoder utilization. Reproduce without network access:

```bash
scripts/video/qualify_gpu_overlap.py \
  --output-dir evidence/video-studio-performance \
  --fixture-dir evidence/video-studio-performance/20260828T051916Z-1130289/fixtures \
  --transcription-model "$SOUNDAR_WHISPER_MODEL_PATH" \
  --faster-whisper-python "$SOUNDAR_FASTER_WHISPER_PYTHON"
```

The production scheduler permits only this measured pairing: one `openai/whisper-tiny` transcription and one non-exclusive NVENC preview/final render reserving at most 2,048 MiB. Larger Whisper models, speech generation, music, image generation, tracking, multiple encoders, and exclusive GPU work remain serialized. A new model/runtime/driver or wider overlap policy requires a fresh three-run report; NVENC’s small standalone delta is never treated as blanket evidence of safety.

## Comparing runs

Use JSON fields rather than parsing console text:

```bash
latest_report="$(find evidence/video-studio-performance -mindepth 2 -maxdepth 2 -name benchmark.json -printf '%T@ %p\n' | sort -nr | head -1 | cut -d' ' -f2-)"
jq '{status, machine, configuration, summary, regression_gate, stages: [.stages[] | {name, status, wall_seconds, realtime_factor, encoder_requested, encoder_actual, gpu, cache}]}' "$latest_report"
```

Run on AC power with the release performance governor, note unrelated GPU processes, and compare at least three repetitions when tuning concurrency. Preserve the raw run directory as release evidence; the repository ignores `evidence/` so large media is not accidentally committed.
