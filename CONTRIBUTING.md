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
- Never include generated packages, caches, local exports, or downloaded models.

By contributing, you agree that your contribution is licensed under the MIT
License included in this repository.
