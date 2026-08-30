# Contributing to soundAr

Thank you for helping improve soundAr. The project welcomes focused bug fixes,
model adapters, performance work, tests, documentation, and accessible UI
improvements.

## Before You Start

- Open an issue before large architectural changes or new model families.
- Keep inference local. Do not add hosted inference, telemetry, or required API keys.
- Do not commit model weights, generated audio, voice samples, credentials, or private keys.
- Only contribute voice data that you own and have permission to use.
- Check the upstream license and terms for every model or dataset you introduce.

## Development

```bash
cd app
npm ci
npm run build
cd src-tauri
cargo test --locked
```

Compile the Python runtime modules from the repository root:

```bash
python3 -m compileall -q bridge.py config core engines
```

For desktop development, run `npm run tauri dev` from `app/`. See the README
for managed Linux runtime setup.

## Pull Requests

- Keep changes scoped and explain the user-visible behavior.
- Add or update tests when behavior changes.
- Run the frontend build, Rust tests, and Python compile check.
- Update `CHANGELOG.md` for release-facing changes.
- Releases advance one patch at a time - `0.8.1`, `0.8.2`, `0.8.3` - including for new features.
  Bump `app/package.json`, `app/package-lock.json`, `app/src-tauri/tauri.conf.json`, and
  `app/src-tauri/Cargo.toml` together, and run `./scripts/check-release-version.sh`. A minor bump is
  reserved for completing a roadmap phase's exit gate. See Version Numbering in `ROADMAP.md`.
- Never include generated packages, caches, local exports, or downloaded models.

By contributing, you agree that your contribution is licensed under the MIT
License included in this repository.
