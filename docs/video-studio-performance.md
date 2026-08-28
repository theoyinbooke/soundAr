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

Reference run: `20260828T030608Z-946269` on 2026-08-28 UTC at harness commit `45ca914b697c648ab9cf83e99f77e4974aa3d981`. The immutable report SHA-256 is `0bff485d6c684d16d4f43c8f971326001e866873b68e18ec9d87772da8c0b396`.

| Component | Baseline |
|---|---|
| OS / kernel | Ubuntu 26.04 LTS / 7.0.0-30-generic |
| CPU | Intel Core i9-13900HX, 32 logical CPUs |
| RAM | 64,204,172 KiB reported by Linux |
| GPU | NVIDIA GeForce RTX 4080 Laptop GPU, compute capability 8.9 |
| Driver / VRAM | 595.84 / 12,282 MiB |
| FFmpeg | 8.0.1-3ubuntu2 |
| Final encoder | H.264 NVENC (`h264_nvenc`, runtime-smoke verified) |
| Tool readiness | FFmpeg/FFprobe/NVENC ready; Node 22.23.2 ready; yt-dlp/EJS and transcription runtime not yet installed |

The benchmark used one 6.000 s moving imported-source fixture and one 5.205 s animated-podcast fixture. Both contain AAC audio; the imported source carries three caption cues with visible source-clock gaps. Final output is 1080×1920.

| Stage | Wall time | RTF | Encoder | Peak VRAM | Peak delta |
|---|---:|---:|---|---:|---:|
| Imported-source probe | 0.074 s | 0.0123 | n/a | 1,264 MiB | 0 MiB |
| Podcast-source probe | 0.065 s | 0.0125 | n/a | 1,264 MiB | 0 MiB |
| 640×360 proxy, cache miss | 0.237 s | 0.0394 | libx264 ultrafast | 1,264 MiB | 0 MiB |
| 640×360 proxy, validated cache hit | 0.139 s | 0.0232 | no render | n/a | n/a |
| 540×960 portrait preview | 0.300 s | 0.0500 | libx264 ultrafast | 1,264 MiB | 0 MiB |
| 1080×1920 imported reel final | 0.863 s | 0.1439 | H.264 NVENC | 1,603 MiB | 339 MiB |
| 1080×1920 animated podcast final | 0.724 s | 0.1393 | H.264 NVENC | 1,603 MiB | 339 MiB |

End-to-end wall time was 5.058 s, including 1.480 s of rights-clear fixture generation, all probes, proxy miss/hit, preview, two final renders, checksums, FFprobe validation, and first-frame decode checks. All ten regression checks passed. The test cache hit ratio is intentionally 0.5 because it issues one miss followed by one hit for the same canonical key.

Output validation evidence:

| Artifact | Bytes | SHA-256 |
|---|---:|---|
| Proxy / cache-hit payload | 818,731 | `49d9a0e160f5b750b5c4889f9edf865d1914dc9bd20695bac1157841aad7c626` |
| Portrait preview | 946,455 | `20c8aa2864cc628339d5bad5aa38f832e34e39eb15ca4aa3d65c229442ce7176` |
| Imported reel final | 5,189,244 | `2a54d590571ebe73b493c43dd2def6ff84310ad173a9d213f38aa890e59488ac` |
| Animated podcast final | 427,521 | `200c20fc60e9c81d282d48eb853d99e428c71733aad427228bdc6fceaafe86a9` |

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
| Cache-hit wall / miss wall | 0.90 |
| Each final-render VRAM delta | 2,048 MiB |

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

The reference run explicitly records `transcription_faster_whisper` as skipped because no managed faster-whisper runtime or local CTranslate2 model was installed. This is visible evidence, not a passing transcription result. The release gate remains incomplete until setup is finished and this succeeds without a model download:

```bash
scripts/video/run-smoke-benchmark.sh \
  --output-dir evidence/video-studio-performance \
  --transcription-model "$SOUNDAR_WHISPER_MODEL_PATH" \
  --faster-whisper-python "$SOUNDAR_FASTER_WHISPER_PYTHON"
```

The transcription artifact must use integer microseconds, preserve gaps, include word timing, and run with VAD disabled. Record model revision/hash, device, precision, wall time, RTF, and VRAM. Qualify inference on an otherwise idle GPU before measuring overlap.

Do not infer that NVENC’s 339 MiB render delta makes arbitrary concurrent inference safe. Until measured with the installed model, Whisper remains a heavy/exclusive workload and must not overlap music, image generation, tracking, or another heavy render. A safe-overlap result requires three clean repetitions with no OOM, stable output contracts, peak total VRAM below the scheduler budget, and no stage exceeding its RTF gate.

## Comparing runs

Use JSON fields rather than parsing console text:

```bash
latest_report="$(find evidence/video-studio-performance -mindepth 2 -maxdepth 2 -name benchmark.json -printf '%T@ %p\n' | sort -nr | head -1 | cut -d' ' -f2-)"
jq '{status, machine, configuration, summary, regression_gate, stages: [.stages[] | {name, status, wall_seconds, realtime_factor, encoder_requested, encoder_actual, gpu, cache}]}' "$latest_report"
```

Run on AC power with the release performance governor, note unrelated GPU processes, and compare at least three repetitions when tuning concurrency. Preserve the raw run directory as release evidence; the repository ignores `evidence/` so large media is not accidentally committed.
