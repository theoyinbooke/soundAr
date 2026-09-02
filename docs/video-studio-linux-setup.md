# Linux Video Studio setup

Video Studio is local-first. FFmpeg performs media work, faster-whisper is the primary internal transcription runtime, and yt-dlp handles one explicitly authorized link at a time. None of the status or benchmark commands below downloads software, contacts a media URL, changes system packages, or requires `sudo`.

## Headless agent mode

The packaged `soundar-desktop` binary exposes the same authenticated Video Studio tools without starting Tauri or opening a window. This is the supported automation boundary for Codex and local scripts; it does not duplicate rendering or mutate the manifest directly.

List the exact installed tool schemas:

```bash
soundar-desktop agent tools --pretty
```

Invoke one tool with a JSON request from an argument or standard input:

```bash
soundar-desktop agent video list_video_projects --request '{}'
printf '%s\n' '{"project_id":"PROJECT_ID"}' \
  | soundar-desktop agent video get_video_project --progress --pretty
```

Final structured results are written to standard output; concise high-level progress is written to standard error when `--progress` is present. Requests are limited to 1 MiB, use the same strict schemas, project locks, version checks, idempotency identifiers, approval boundaries, durable jobs, and registered artifact projection as the desktop UI and assistant. A headless caller must never set rights or `user_confirmed` flags on the user's behalf. User-selected image approval is valid only after a native/user-controlled selection; locally generated images instead carry exact producer and generation provenance through `add_visual_asset`.

## Check this machine first

From the repository root:

```bash
scripts/video/check-toolchain.sh
scripts/video/check-toolchain.sh --nvenc-smoke --json
```

The first command is read-only. `--nvenc-smoke` additionally encodes two locally generated 256×256 frames to a null sink. It neither reads user media nor publishes a file.

Discovery considers explicitly configured paths, the current `PATH`, normal `/usr` and `/usr/local` locations, `~/.local/bin`, Snap, NVM, mise/asdf shims, Deno’s user directory, and soundAr-managed runtime directories. The app must persist an absolute configured path when a desktop launch environment cannot see an NVM or user-shell path.

Supported path overrides are:

| Component | Override |
|---|---|
| FFmpeg | `SOUNDAR_FFMPEG_PATH` |
| FFprobe | `SOUNDAR_FFPROBE_PATH` |
| yt-dlp | `SOUNDAR_YT_DLP_PATH` |
| Node | `SOUNDAR_NODE_PATH` |
| Deno | `SOUNDAR_DENO_PATH` |
| faster-whisper Python | `SOUNDAR_FASTER_WHISPER_PYTHON` |
| local CTranslate2 model | `SOUNDAR_WHISPER_MODEL_PATH` |
| whisper.cpp CLI | `SOUNDAR_WHISPER_CPP_PATH` |
| NVIDIA status tool | `SOUNDAR_NVIDIA_SMI_PATH` |
| stable-diffusion.cpp `sd-cli` (video generator) | `SOUNDAR_SD_CLI_PATH` |
| MiniMax H3 weights directory | `SOUNDAR_H3_MODEL_DIR` |

An override is accepted only when it resolves to an executable regular file. A discovered encoder is not considered usable until its runtime smoke test succeeds.

## FFmpeg and FFprobe

The supported renderer needs:

- FFmpeg and FFprobe from the same build family;
- H.264 through `h264_nvenc` when the NVIDIA runtime smoke passes, with `libx264` as the required fallback;
- `subtitles`/libass for caption burn-in;
- `scale`, `crop`, `pad`, `overlay`, `showwaves`, AAC, and MP4 support;
- CUDA/NVDEC/NVENC capabilities when available, but no hard dependency on a GPU for correctness.

Inspect the exact build rather than assuming a distribution package has every feature:

```bash
ffmpeg -hide_banner -version
ffmpeg -hide_banner -encoders | grep -E 'h264_nvenc|hevc_nvenc|av1_nvenc|libx264'
ffmpeg -hide_banner -filters | grep -E 'subtitles|ass|showwaves|overlay'
ffmpeg -hide_banner -hwaccels
```

Video Studio falls back to libx264 when NVENC is absent or the runtime smoke fails. It must surface the reason; it must not silently claim a hardware render. See the [FFmpeg command reference](https://ffmpeg.org/ffmpeg.html) for the underlying options.

## Managed yt-dlp and yt-dlp-ejs

Link preview and import are never prerequisites for local upload. They remain unavailable until yt-dlp, an EJS package, and a supported JavaScript runtime are all ready.

As of the audited 2026 baseline, yt-dlp’s [EJS setup guide](https://github.com/yt-dlp/yt-dlp/wiki/EJS) supports Deno 2.3+ by default and Node 22+ when explicitly enabled. This machine already has Node 22.23.2, so the smallest managed setup is a Python environment containing the matching yt-dlp default dependency group plus an absolute Node path. The audited pair is yt-dlp 2026.6.9 with yt-dlp-ejs 0.8.0.

The following is an explicit networked setup step. Review package versions and provenance before running it; normal app startup never runs these commands:

```bash
soundar_ytdlp_runtime="${XDG_DATA_HOME:-$HOME/.local/share}/soundar/runtimes/yt-dlp-2026.6.9"
soundar_ytdlp_wheels="${XDG_CACHE_HOME:-$HOME/.cache}/soundar/wheels/yt-dlp-2026.6.9"
python3 -m venv "$soundar_ytdlp_runtime"
mkdir -p "$soundar_ytdlp_wheels"
"$soundar_ytdlp_runtime/bin/python" -m pip download \
  --dest "$soundar_ytdlp_wheels" \
  'yt-dlp[default]==2026.6.9' \
  'yt-dlp-ejs==0.8.0'
"$soundar_ytdlp_runtime/bin/python" -m pip install \
  --no-index --find-links "$soundar_ytdlp_wheels" \
  'yt-dlp[default]==2026.6.9' \
  'yt-dlp-ejs==0.8.0'
sha256sum "$soundar_ytdlp_wheels"/* >"$soundar_ytdlp_runtime/wheelhouse.sha256"
"$soundar_ytdlp_runtime/bin/python" -m pip freeze --all >"$soundar_ytdlp_runtime/runtime.lock"
```

This separates the one-time reviewed download from offline installation and records package resolution and wheel hashes. Release packaging should ship an audited wheelhouse or signed official executable; it should not resolve “latest” at runtime.

Configure absolute paths in soundAr or its launching environment:

```bash
export SOUNDAR_YT_DLP_PATH="$soundar_ytdlp_runtime/bin/yt-dlp"
export SOUNDAR_NODE_PATH="$(command -v node)"
"$SOUNDAR_YT_DLP_PATH" --version
"$SOUNDAR_YT_DLP_PATH" --js-runtimes "node:$SOUNDAR_NODE_PATH" --help >/dev/null
scripts/video/check-toolchain.sh
```

Installing `yt-dlp[default]` and `yt-dlp-ejs` into the same environment keeps EJS local. Do not enable `--remote-components` as the product default. The official standalone Linux/Unix executables can bundle EJS, but the offline checker reports that state as “possible” until an explicitly authorized metadata preview verifies it.

Deno is a supported alternative. Use the [official Deno installation instructions](https://docs.deno.com/runtime/getting_started/installation/), verify version 2.3 or newer, and configure its absolute path. Do not install both runtimes merely to make the status screen green.

Link safety is independent of tool readiness:

- preview is metadata-only;
- import accepts one exact URL and rejects playlist/bulk defaults;
- the rights checkbox starts unchecked for every URL change;
- the persisted rights receipt must match that exact URL;
- cookies, credentials, and browser profiles are never inferred or copied automatically.

## Managed faster-whisper

The selected primary runtime is [faster-whisper](https://github.com/SYSTRAN/faster-whisper): it supports word timestamps, local CTranslate2 model directories, CUDA compute, and reduced precision. whisper.cpp remains an optional CPU-oriented fallback, not a second shadow workflow.

The current faster-whisper GPU runtime requires CUDA 12 cuBLAS and CUDA 12 cuDNN 9. Keep these libraries inside the managed environment. Do not add global library symlinks.

The following is another explicit networked setup step. These exact CUDA wheel versions match the qualified RTX 4080 runtime:

```bash
soundar_whisper_runtime="${XDG_DATA_HOME:-$HOME/.local/share}/soundar/runtimes/faster-whisper-1.2.1"
soundar_whisper_wheels="${XDG_CACHE_HOME:-$HOME/.cache}/soundar/wheels/faster-whisper-1.2.1"
python3 -m venv "$soundar_whisper_runtime"
mkdir -p "$soundar_whisper_wheels"
"$soundar_whisper_runtime/bin/python" -m pip download \
  --dest "$soundar_whisper_wheels" \
  'faster-whisper==1.2.1' \
  'nvidia-cublas-cu12==12.4.5.8' \
  'nvidia-cudnn-cu12==9.1.0.70'
"$soundar_whisper_runtime/bin/python" -m pip install \
  --no-index --find-links "$soundar_whisper_wheels" \
  'faster-whisper==1.2.1' \
  'nvidia-cublas-cu12==12.4.5.8' \
  'nvidia-cudnn-cu12==9.1.0.70'
sha256sum "$soundar_whisper_wheels"/* >"$soundar_whisper_runtime/wheelhouse.sha256"
"$soundar_whisper_runtime/bin/python" -m pip freeze --all >"$soundar_whisper_runtime/runtime.lock"
```

Keep `runtime.lock` and the wheelhouse hashes with the release evidence. Re-qualification is required before changing CTranslate2, cuBLAS, cuDNN, the driver, or the selected model.

Materialize an audited CTranslate2 model into a local directory using the soundAr model manager or a reviewed offline transfer. Record its upstream revision, license, total bytes, and file hashes. Never pass a Hub model name to the background service: a name can trigger an implicit download. For this 12 GiB GPU, `distil-large-v3` is the throughput-oriented starting profile and `large-v3` is the quality profile; qualify both before changing the default.

Configure the local runtime and model:

```bash
export SOUNDAR_FASTER_WHISPER_PYTHON="$soundar_whisper_runtime/bin/python"
export SOUNDAR_WHISPER_MODEL_PATH="${XDG_DATA_HOME:-$HOME/.local/share}/soundar/models/whisper/distil-large-v3-ct2"
soundar_whisper_libs="$("$SOUNDAR_FASTER_WHISPER_PYTHON" -c 'import os,nvidia.cublas.lib,nvidia.cudnn.lib; print(os.path.dirname(nvidia.cublas.lib.__file__) + ":" + os.path.dirname(nvidia.cudnn.lib.__file__))')"
LD_LIBRARY_PATH="$soundar_whisper_libs${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
  "$SOUNDAR_FASTER_WHISPER_PYTHON" -c 'import ctranslate2; print(ctranslate2.get_cuda_device_count())'
scripts/video/check-toolchain.sh
```

The app’s managed process launcher must supply that library path only to the transcription child. The harness requires an existing model directory and uses `vad_filter=False`, integer microsecond output, word timestamps, and explicit source gaps:

```bash
scripts/video/run-smoke-benchmark.sh \
  --output-dir evidence/video-studio-performance \
  --transcription-model "$SOUNDAR_WHISPER_MODEL_PATH" \
  --faster-whisper-python "$SOUNDAR_FASTER_WHISPER_PYTHON"
```

For a CPU fallback, build the official [whisper.cpp](https://github.com/ggml-org/whisper.cpp) `whisper-cli`, configure its absolute path, and keep the model local. Do not let fallback timestamps define a different timeline contract.

## Troubleshooting

| Symptom | Check | Safe response |
|---|---|---|
| FFmpeg exists but Studio says unavailable | Compare configured FFmpeg/FFprobe paths and versions | Configure an absolute matching pair; do not replace system binaries from the app |
| NVENC is listed but its smoke fails | Driver error in status JSON; `nvidia-smi` visibility | Use libx264 and surface the error; repair the driver outside soundAr |
| Captions fail | `subtitles` and `ass` filters; font availability | Use a libass-enabled build or soft captions; do not publish a silent failure |
| yt-dlp reports missing challenge support/formats | yt-dlp/EJS versions, Node 22+ or Deno 2.3+, absolute runtime path | Upgrade yt-dlp and EJS together in the managed environment |
| Link import works in a shell but not the app | Desktop process may not inherit NVM paths | Persist absolute Node and yt-dlp paths shown by the checker |
| `libcudnn.so.9` or `libcublas.so` cannot load | Managed wheel install and child `LD_LIBRARY_PATH` | Fix the child environment; never create global symlinks |
| faster-whisper starts downloading | A model name was passed instead of a directory | Cancel and select an audited local model directory |
| Media work exhausts VRAM | Per-stage peak/delta; concurrent heavy jobs | Keep all workloads outside the documented Whisper-tiny + one-NVENC envelope serialized |
| Cache hit is not faster than a miss | Key, checksum, FFprobe validation, publication mode | Treat it as a failed performance gate; do not bypass validation |

## Local smoke and release evidence

```bash
scripts/video/test-harness.sh
scripts/video/run-smoke-benchmark.sh --output-dir evidence/video-studio-performance
scripts/video/qualify_gpu_overlap.py \
  --output-dir evidence/video-studio-performance \
  --fixture-dir "$SOUNDAR_VIDEO_FIXTURE_DIR" \
  --transcription-model "$SOUNDAR_WHISPER_MODEL_PATH" \
  --faster-whisper-python "$SOUNDAR_FASTER_WHISPER_PYTHON"
```

The harness synthesizes all media locally with FFmpeg lavfi, includes a two-second speech gap, validates every output through FFprobe and a first-frame decode, measures GPU/VRAM, verifies content-addressed cache reuse, and atomically publishes an immutable JSON report. See [video-studio-performance.md](video-studio-performance.md) for the exact-machine baseline and regression thresholds.

## Footage for a performed episode

A show performed by voices has nothing to film. Where `sd-cli` and a MiniMax H3 distilled
checkpoint are installed, soundAr generates short shots in the episode's own aspect, set in the
world the show's look describes, and cuts them across the narration. Where they are not, it draws a
motion backdrop with FFmpeg alone: a drifting field in the show's palette under grain and a
vignette, with the title and a speaker card burned in. `sd-cli` is discovered like FFmpeg and
reported in runtime status whether present or absent; the weights are never bundled, and the
directory is found through `SOUNDAR_H3_MODEL_DIR`, `~/.soundAr/models/minimax-h3`, or a registry
entry with engine `minimax-h3`.
