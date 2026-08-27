<p align="center">
  <img src="app/public/brand/soundar-app-icon.svg" alt="soundAr" width="112" />
</p>

# soundAr

soundAr is an open-source, local-first desktop studio for generating, cloning,
comparing, and benchmarking voices, plus bounded short music drafts, with local models.

[![License: MIT](https://img.shields.io/badge/License-MIT-f0a928.svg)](LICENSE)
[![CI](https://github.com/theoyinbooke/soundAr/actions/workflows/ci.yml/badge.svg)](https://github.com/theoyinbooke/soundAr/actions/workflows/ci.yml)

> Model availability does not imply a universal open-source or commercial-use
> license. Review each provider's current terms before downloading or distributing
> model artifacts. See [Model and Data Licenses](MODEL_LICENSES.md).

## Desktop App

The current desktop experience lives in `app/` and uses React 19, Vite 7, and Tauri 2. Tauri keeps the native shell small while a bounded pool of isolated Python workers reuses engine adapters and keeps recently used models warm between generations.

```bash
cd app
npm install
npm run tauri dev
```

For frontend-only development with simulated synthesis results:

```bash
cd app
npm run dev
```

Open `http://localhost:1421`.

## Linux Installation

Install the latest Debian release and its managed CUDA runtime:

```bash
curl -fsSLO https://github.com/theoyinbooke/soundAr/releases/latest/download/install-linux.sh
chmod +x install-linux.sh
./install-linux.sh
```

To install a locally built package, pass its path:

```bash
./install-linux.sh app/src-tauri/target/release/bundle/deb/soundAr_0.4.0_amd64.deb
```

When the Debian package or AppImage is installed directly, soundAr detects a missing Python
environment and offers the same managed runtime setup inside the application. Setup is user-space,
retryable, and keeps model weights across application upgrades.

Starting with version 0.2.2, the app checks signed GitHub Releases shortly after launch and every
six hours. AppImage installations can update and restart in place; Debian installations receive an
in-app release notice and continue through the system package installer.

The desktop package contains the versioned engine code. Python 3.11 and model libraries live in
`${XDG_DATA_HOME:-$HOME/.local/share}/soundar/runtime`, while downloaded model weights and exports
remain under `~/.soundAr`. Upgrading the desktop does not redownload models.

### Model Downloads

soundAr installers and application updates do not contain model weights. After runtime setup, the
user chooses a model in the Models screen and approves its separate download from the documented
upstream provider. Before downloading, soundAr must show the source, pinned revision, license and
access conditions, expected disk use, and hardware requirements. Installing, launching, or updating
soundAr must never start a model-weight download in the background.

The Models screen records the resolved upstream commit and file-size manifest for each new
installation. Health checks revalidate those files before an engine can use them. A damaged install
stays visible as **Repair needed**, can be repaired from its pinned revision, or can be removed
without deleting projects and generated audio. Older installs receive a structural compatibility
check until they are repaired or reinstalled with a pinned manifest.

The managed runtime is separate from model content: runtime setup may download Python, PyTorch, and
engine libraries after the user starts setup. Already-downloaded models remain local and are not
removed or replaced by an application update.

Before a SQLite schema upgrade, soundAr creates and verifies a consistent backup beside
`soundar.sqlite3`, including committed WAL data. Startup also runs an integrity check before and
after migration. If that check fails, soundAr leaves the database untouched and reports the exact
database and backup directory instead of silently resetting local work.

Generated audio is registered with its byte length and SHA-256 checksum. History reports missing or
size-modified files immediately, and playback verifies the checksum before returning audio so a file
changed outside soundAr is never presented as the original generation.

## Architecture

- `app/src/`: compact React workspace with a neutral light-first desktop design system and an optional dark appearance
- `app/src-tauri/`: SQLite state, GPU-aware parallel scheduler, native audio, local API, and desktop integration
- `bridge.py`: persistent JSON-lines inference worker with an engine-scoped warm model cache
- `core/`: model registry, audio utilities, benchmarking, and unified TTS/STT/music APIs
- `engines/`: model-specific open-source inference adapters
- `data/curated_models.json`: curated local model catalog

The application performs inference locally after model download. There is no cloud inference, API-key dependency, telemetry requirement, or online fallback.

### Text-to-Music Beta

Music generation uses two deliberately separate text conditions: **Music direction**
describes the intended genre, instruments, arrangement, mood, tempo, and vocal character;
optional **Lyrics or text to sing** supplies the words and section markers such as
`[Verse]` and `[Chorus]`. They are persisted independently in the durable request and
History record, so a retry never turns a lyric into a style prompt or vice versa.

ACE-Step 1.5 XL Turbo is the local lyric-conditioned route. It uses an isolated runtime,
a user-approved pinned checkpoint, and the official local Diffusers pipeline to render
short 48 kHz stereo WAV or FLAC music. Lyrics are a generation condition—not a transcript
guarantee—so every render needs a listening review. ACE-Step requires CUDA in this release;
the 12 GB target uses model CPU offload and must pass the packaged GPU acceptance gate before
release.

MusicGen Small remains available for short 32 kHz instrumental drafts. It cannot condition
on lyrics, and soundAr rejects a lyric request sent to it instead of pretending the text was
sung. Its weights are CC BY-NC 4.0, so it must not be presented as commercial-use ready.
Source-audio cover/remix/repaint, melody conditioning, voice/reference uploads, and batch
music are deliberately out of scope until each has its own consent, safety, and hardware
qualification.

### Parallel Jobs and Batch API

Independent generations and batch rows run through the same durable scheduler. The default ceiling is four concurrent workers and can be changed from 1 to 8 with `SOUNDAR_MAX_PARALLEL_JOBS`; actual GPU admission is additionally bounded by each engine's declared VRAM envelope and current free memory. Low, normal, high, and urgent priority are durable across restarts. Waiting work ages upward one effective level every 30 seconds so repeated urgent submissions cannot permanently starve older jobs. Every batch row has its own job, status, error, history artifact, and cancellation target.

Batch execution uses a guarded rolling queue: only one coordinator can own a batch, and each worker takes the next row as soon as it is free. `GET /v1/runtime/scheduler` (or `./soundar_cli.py scheduler`) reports active workers, the configured ceiling, reserved VRAM, and active batches.

After explicitly starting the loopback API in Settings, submit and monitor a batch with:

```bash
export SOUNDAR_API_TOKEN='<token shown by soundAr>'
./soundar_cli.py batch scripts.txt --parallelism 2 --priority high
```

The Batch workspace and CLI accept TXT (one item per non-empty line), CSV, and JSONL. Structured
files can set a row name, deterministic output name, and supported model settings without changing
the batch defaults. See [Batch input formats](docs/batch-formats.md) for examples and validation
rules.

Batch pause is graceful: active rows finish while queued rows remain restart-safe. Resume queued
work or explicitly retry failed rows without regenerating successful artifacts:

```bash
./soundar_cli.py pause-batch <batch-id>
./soundar_cli.py resume-batch <batch-id> --parallelism 2
./soundar_cli.py resume-batch <batch-id> --retry-failed
```

Failed or cancelled standalone generations are retryable with their original settings, while
finished queue rows can be dismissed without deleting history or audio:

```bash
./soundar_cli.py retry <job-id>
./soundar_cli.py clear-finished
```

For non-blocking integrations, queue an idempotent speech job and optionally wait for its verified
artifact. Reusing a key with the same request returns the original job; using it for different
content is rejected:

```bash
./soundar_cli.py queue "A durable local generation" --output output.wav
./soundar_cli.py queue "A durable local generation" --idempotency-key my-request --no-wait
./soundar_cli.py job <job-id>
```

Local clients can follow durable progress without polling through
`GET /v1/jobs/<job-id>/events`, a resumable server-sent event stream. Generated audio remains an
atomic completed artifact; chunked audio streaming is not advertised by engines that have not
passed latency and soak qualification.

Inference workers write to a managed `.partial` file. The native store validates the audio,
atomically publishes it, records its size and SHA-256 checksum, and only then completes the job.
On restart, interrupted publications are rolled back and shown as retryable failures rather than
appearing as valid history.

The versioned API contract is in [`docs/openapi.yaml`](docs/openapi.yaml). Model weights are not bundled with the app or release artifacts.

On NVIDIA systems, the runtime uses CUDA 12.4 PyTorch wheels, keeps the selected model warm,
enables TF32 on supported GPUs, and uses the expandable CUDA allocator to reduce fragmentation.

## Legacy PyQt App

The original PyQt interface remains available while the Tauri desktop app matures:

```bash
bash install.sh
source .venv/bin/activate
python3 main.py
```

## Verification

```bash
./scripts/check-release-version.sh
python3.11 -m venv .test-venv
.test-venv/bin/pip install -r requirements-test.txt
.test-venv/bin/python -m unittest discover -s tests -v
cd app
npm test
npm run test:e2e
npm run build
../scripts/verify-production-boundary.sh
npm run test:production
cd src-tauri
cargo test --locked
```

On a prepared NVIDIA machine with the qualified models installed, run the
release-candidate model-switch and OOM-recovery soak with:

```bash
SOUNDAR_SOAK_DURATION_SECONDS=1800 ./scripts/run-packaged-gpu-soak.sh
```

The command writes machine-readable evidence under `evidence/`, which is ignored
because it contains machine-specific paths and runtime identifiers. A shortened
duration verifies the harness only; release evidence requires the full 30 minutes.

The owned evidence layers and manual release gates are defined in
[`docs/test-matrix.md`](docs/test-matrix.md) and
[`docs/release-checklist.md`](docs/release-checklist.md).

## Releases

Linux releases are created from version tags. Keep the version in `app/package.json`,
`app/src-tauri/Cargo.toml`, and `app/src-tauri/tauri.conf.json` aligned, then push a matching tag:

```bash
git tag -a v0.4.0 -m "soundAr v0.4.0"
git push origin v0.4.0
```

GitHub Actions tests the source, builds signed Debian and AppImage artifacts into a draft release,
verifies their contents, generates checksums and build provenance, and only then publishes the
release. See `CHANGELOG.md` for release details.

## Open Source

soundAr source code and bundled brand assets are available under the [MIT License](LICENSE).
Contributions are welcome; read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request.
Please report vulnerabilities through the private process in [SECURITY.md](SECURITY.md).

The prioritized product plan, architecture milestones, acceptance tests, and release gates are in
[ROADMAP.md](ROADMAP.md).
