<p align="center">
  <img src="app/public/brand/soundar-app-icon.svg" alt="soundAr" width="112" />
</p>

# soundAr

soundAr is an open-source, local-first desktop studio for generating, cloning,
comparing, and benchmarking voices with local speech models.

[![License: MIT](https://img.shields.io/badge/License-MIT-f0a928.svg)](LICENSE)
[![CI](https://github.com/theoyinbooke/soundAr/actions/workflows/ci.yml/badge.svg)](https://github.com/theoyinbooke/soundAr/actions/workflows/ci.yml)

> Model availability does not imply a universal open-source or commercial-use
> license. Review each provider's current terms before downloading or distributing
> model artifacts. See [Model and Data Licenses](MODEL_LICENSES.md).

## Desktop App

The current desktop experience lives in `app/` and uses React 19, Vite 7, and Tauri 2. Tauri keeps the native shell small while a persistent Python sidecar reuses the existing engine adapters and keeps the active model warm between generations.

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
./install-linux.sh app/src-tauri/target/release/bundle/deb/soundAr_0.2.4_amd64.deb
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

## Architecture

- `app/src/`: compact React workspace with dark and cream-light themes
- `app/src-tauri/`: native window, hardware discovery, local file access, and command bridge
- `bridge.py`: persistent JSON-lines Python runtime with a single warm model cache
- `core/`: model registry, audio utilities, benchmarking, and unified TTS/STT APIs
- `engines/`: model-specific open-source inference adapters
- `data/curated_models.json`: curated local model catalog

The application performs inference locally after model download. There is no cloud inference, API-key dependency, telemetry requirement, or online fallback.

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
cd app
npm run build
cd src-tauri
cargo test --locked
```

## Releases

Linux releases are created from version tags. Keep the version in `app/package.json`,
`app/src-tauri/Cargo.toml`, and `app/src-tauri/tauri.conf.json` aligned, then push a matching tag:

```bash
git tag -a v0.2.4 -m "soundAr v0.2.4"
git push origin v0.2.4
```

GitHub Actions tests the source, builds signed Debian and AppImage artifacts into a draft release,
verifies their contents, generates checksums and build provenance, and only then publishes the
release. See `CHANGELOG.md` for release details.

## Open Source

soundAr source code and bundled brand assets are available under the [MIT License](LICENSE).
Contributions are welcome; read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request.
Please report vulnerabilities through the private process in [SECURITY.md](SECURITY.md).
