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
| WavLM Base Plus SV | `feb593a6c23c1cc3d9510425c29b0a14d2b07b1e` | CC BY-SA 3.0 per the upstream license link | No |
| Wav2Vec2 Base 960h | `22aad52d435eb6dbaf354bdad9b0da84ce7d6156` | Apache 2.0 | No |

The table records the revision tested by soundAr. The installer still retrieves
the model from its upstream provider only after explicit review and approval.
