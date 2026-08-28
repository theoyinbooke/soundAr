# Model and Data Licenses

The MIT License in this repository covers soundAr's original source code and
bundled brand assets. It does not relicense third-party models, datasets, Python
packages, system libraries, or generated voice content.

soundAr downloads model artifacts from their upstream providers. Each model is
governed by its own license, acceptable-use policy, access conditions, and
jurisdictional requirements. A model appearing in the curated catalog is a
compatibility statement, not a representation that the model is open source or
approved for every commercial or personal use.

Before downloading or distributing a model, review its current upstream model
card and license. Gated models may require separate acceptance or authentication.
Contributors adding a model must document its upstream source and must not commit
weights or datasets to this repository.

## Distribution Policy

soundAr does not redistribute model weights in its source repository, Debian
package, AppImage, application update, or default runtime bundle. Installing,
launching, updating, or setting up the managed Python runtime must not silently
download model weights.

Model installation is a separate, explicit user action. Before a download begins,
the application must identify the upstream provider, pinned revision, license and
access conditions, expected download and installed size, and relevant hardware
requirements. Gated models remain subject to the provider's own authentication and
acceptance flow. Downloads should be verified against their recorded revision and
available checksums before an installation is marked ready.

Python, PyTorch, CUDA-compatible wheels, engine libraries, and small language
processing packages are runtime dependencies rather than model weights. soundAr
may download them only after the user starts managed runtime setup, with the
operation and expected storage cost clearly presented.

Application upgrades must preserve locally installed models. Future offline model
packs may be supported as separate user-imported artifacts only when redistribution
is permitted; they must never become an implicit part of the desktop installer.

Users are responsible for obtaining consent to process or clone a voice and for
complying with privacy, publicity, copyright, biometric-data, and synthetic-media
rules that apply to their use.

## Qualified Sources

| Model | Qualified revision | Upstream license | Weights bundled |
| --- | --- | --- | --- |
| Chatterbox Turbo | `749d1c1a46eb10492095d68fbcf55691ccf137cd` | MIT | No |
| Breeze TTS 2 | `c1c8ca18b70b30822735633991d9ebf4898e47d4` | BreezeBlue Research and Non-Commercial License 1.0 | No |
| Fish Speech 1.5 | `275a984d33c33659e39eed41ff5bcd6e67517f4c` | CC BY-NC-SA 4.0 | No |
| MusicGen Small | `47e682ccac550edc80b042ca977074aee86306e7` | CC BY-NC 4.0 | No |
| ACE-Step 1.5 XL Turbo | `200ba991ae448051e14b0183157e35c2d27c9fb0` | MIT for ACE-Step weights; Apache 2.0 for bundled Qwen3 text encoder | No |
| WavLM Base Plus SV | `feb593a6c23c1cc3d9510425c29b0a14d2b07b1e` | CC BY-SA 3.0 per the upstream license link | No |
| Wav2Vec2 Base 960h | `22aad52d435eb6dbaf354bdad9b0da84ce7d6156` | Apache 2.0 | No |

The table records the revision tested by soundAr. The installer still retrieves
the model from its upstream provider only after explicit review and approval.

Breeze TTS 2's model license is not an open-source license. It permits research,
personal, educational, hobbyist, and limited evaluation use, but prohibits
commercial use of the weights, derivative models, and self-hosted outputs without
written authorization from RESONIA, INC. The separately downloaded inference
source is Apache-2.0 and is pinned and checksum-verified by soundAr.

Fish Speech 1.5 model weights are CC BY-NC-SA 4.0: commercial use is not
permitted, attribution is required, and adapted weights must use the same
license. The v1.5.1 inference source is separately Apache-2.0 licensed and is
pinned and checksum-verified by soundAr. Fish Audio S2 Pro uses the newer Fish
Audio Research License and is not qualified for this 12 GB target.
