# Changelog

## 0.2.5 - 2026-08-12

- Made release checksums portable after downloading assets from GitHub.
- Added a clean-room download and checksum verification gate before release publication.

## 0.2.4 - 2026-08-12

- Opened soundAr under the MIT License with contribution, security, and model-license guidance.
- Hardened repository ignores, CI permissions, issue templates, and dependency updates.
- Added guarded signed releases with source tests, package inspection, checksums, provenance, and draft verification.
- Replaced mutable runtime-manager bootstrapping with a pinned, checksum-verified uv download.

## 0.2.3 - 2026-08-11

- Allowed generated `blob:` audio previews in the packaged desktop security policy.
- Added audio header validation before handing generated files to the media decoder.
- Connected History play controls to generated files with loading, pause, and error states.

## 0.2.2 - 2026-08-11

- Added in-app setup when a direct package install has no managed Python runtime.
- Bundled one idempotent runtime bootstrapper for the app and Linux installer.
- Added visible setup progress, retry handling, and synthesis readiness guards.
- Added CUDA and CPU-specific PyTorch installation paths.
- Moved Kokoro English language data into setup to avoid a first-generation download.
- Promoted required Debian runtime tools from recommendations to dependencies.
- Added signed GitHub Release update checks with AppImage install-and-restart support.

## 0.2.1 - 2026-08-11

- Added the soundAr symbol and wordmark across the desktop interface.
- Added dark, light, white, and single-color brand asset variants.
- Added a responsive About view with application and local runtime details.
- Rebuilt desktop, mobile, and installer icons from the new master artwork.

## 0.2.0 - 2026-08-11

- Rebuilt the desktop experience with React, Vite, and Tauri.
- Added real local synthesis through a persistent Python model worker.
- Added generated-audio playback, seeking, and duration-scaled waveform progress.
- Added compact dark and cream-light interfaces across the workspace.
- Added managed Python 3.11 and CUDA 12.4 Linux installation.
- Added Debian, AppImage, CI, and tagged GitHub release automation.
