#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

search_regex() {
  if command -v rg >/dev/null 2>&1; then
    rg -n "$1" "${@:2}"
  else
    grep -REn --exclude='*.pyc' "$1" "${@:2}"
  fi
}

contains_fixed() {
  if command -v rg >/dev/null 2>&1; then
    rg -q --fixed-strings "$1" "${@:2}"
  else
    grep -RFq --exclude='*.pyc' -- "$1" "${@:2}"
  fi
}

qualified_legacy_engines=(
  requirements-engines/breeze.txt
  requirements-engines/fish-speech.txt
)
runtime_requirement_files=(requirements.txt requirements-runtime.txt)
while IFS= read -r requirement_file; do
  runtime_requirement_files+=("$requirement_file")
done < <(
  find requirements-engines -maxdepth 1 -type f \
    ! -name breeze.txt \
    ! -name fish-speech.txt \
    ! -name acestep.txt \
    -print | sort
)

if search_regex \
  'transformers==4\.|transformers[><=]+4\.|diffusers==0\.29|TTS==0\.22|trust_remote_code[[:space:]]*=[[:space:]]*True' \
  "${runtime_requirement_files[@]}"; then
  printf 'Runtime security policy rejected a vulnerable framework pin or executable model code.\n' >&2
  exit 1
fi

# Breeze TTS 2 and Fish Speech 1.5 are isolated compatibility runtimes whose
# qualified upstream stacks currently require Transformers 4.57.3. Keep this
# exception narrow: exact files, exact pins, standalone environments, pinned
# source archives, local model loading, and no model-supplied Python execution.
for requirement_file in "${qualified_legacy_engines[@]}"; do
  [[ "$(grep -Ec '^transformers==4\.57\.3$' "$requirement_file")" == "1" ]] \
    || { printf 'Qualified compatibility pin changed: %s\n' "$requirement_file" >&2; exit 1; }
  if [[ "$(grep -Ec '^transformers' "$requirement_file")" != "1" ]]; then
    printf 'Unexpected Transformers pin in qualified compatibility runtime: %s\n' "$requirement_file" >&2
    exit 1
  fi
done

# ACE-Step 1.5 is an isolated, source-pinned runtime whose official inference
# stack currently requires Transformers 4.57.6. Keep the exception exact and
# verify that both upstream source and local model loading remain constrained.
[[ "$(grep -Ec '^transformers==4\.57\.6$' requirements-engines/acestep.txt)" == "1" ]] \
  || { printf 'Qualified ACE-Step Transformers pin changed.\n' >&2; exit 1; }
[[ "$(grep -Ec '^transformers' requirements-engines/acestep.txt)" == "1" ]] \
  || { printf 'Unexpected Transformers pin in the ACE-Step runtime.\n' >&2; exit 1; }
contains_fixed 'ACESTEP_SOURCE_REVISION="14c0211d5a0653b0f63e27686f4c3f151b4d8629"' setup-engine-runtime.sh \
  || { printf 'The qualified ACE-Step source revision changed.\n' >&2; exit 1; }
contains_fixed 'ACESTEP_SOURCE_SHA256="cdf69c060ed3a6bfddebbf21dd0c548ea7ddfdf0f3cebc20d2a572085970586e"' setup-engine-runtime.sh \
  || { printf 'The qualified ACE-Step source archive checksum changed.\n' >&2; exit 1; }
contains_fixed 'local_files_only=True' engines/music/acestep.py \
  || { printf 'ACE-Step must continue loading only verified local model assets.\n' >&2; exit 1; }
contains_fixed 'if [[ "$ENGINE" == "breeze" || "$ENGINE" == "fish-speech" ]]; then' setup-engine-runtime.sh \
  || { printf 'Qualified compatibility runtimes must remain standalone.\n' >&2; exit 1; }
contains_fixed 'BREEZE_SOURCE_REVISION="ca632ce6c4d05f7985da4eab29b1a5d445b43f7b"' setup-engine-runtime.sh \
  || { printf 'The qualified Breeze source revision changed.\n' >&2; exit 1; }
contains_fixed 'FISH_SOURCE_REVISION="58046eaa1a4cefb0c8cc3a3a667b34186ea02dde"' setup-engine-runtime.sh \
  || { printf 'The qualified Fish Speech source revision changed.\n' >&2; exit 1; }
contains_fixed 'local_files_only=True' engines/tts/breeze_tts.py \
  || { printf 'Breeze must continue loading only verified local model assets.\n' >&2; exit 1; }

# Fish Speech was the last engine left on torch 2.4.1, which honours pickled payloads through
# torch.load even with weights_only=True (CVE-2025-32434), and on hydra-core 1.3.2, whose
# instantiate() executes untrusted config (CVE-2026-68508). Hold both floors here so a future
# requirements edit cannot quietly reintroduce them.
[[ "$(grep -Ec '^torch==2\.(6|7|8|9|1[0-9])\.' requirements-engines/fish-speech.txt)" == "1" ]] \
  || { printf 'Fish Speech must stay on torch 2.6.0 or newer (CVE-2025-32434).\n' >&2; exit 1; }
[[ "$(grep -Ec '^hydra-core==1\.3\.(4|[5-9])$' requirements-engines/fish-speech.txt)" == "1" ]] \
  || { printf 'Fish Speech must stay on hydra-core 1.3.4 or newer (CVE-2026-68508).\n' >&2; exit 1; }
if search_regex '^torch==2\.[0-5]\.' requirements-engines requirements.txt requirements-runtime.txt; then
  printf 'Runtime security policy rejected a torch pin below 2.6.0 (CVE-2025-32434).\n' >&2
  exit 1
fi

# The isolated Breeze, Fish Speech, and ACE-Step runtimes stay on Transformers 4.57.x, which
# resolves and executes remote kernel code named by a checkpoint config (CVE-2026-4372). The
# download gate that refuses such a checkpoint is the compensating control, so require it.
contains_fixed 'def unsafe_config_fields' core/model_assets.py \
  || { printf 'The dynamic-kernel config gate is missing from core/model_assets.py.\n' >&2; exit 1; }
contains_fixed 'unsafe_fields = unsafe_config_fields(target_dir)' core/model_manager.py \
  || { printf 'The dynamic-kernel config gate is no longer called during model installs.\n' >&2; exit 1; }
contains_fixed '_attn_implementation_internal' core/model_assets.py \
  || { printf 'The dynamic-kernel config gate lost its CVE-2026-4372 field check.\n' >&2; exit 1; }

if search_regex 'trust_remote_code[[:space:]]*=[[:space:]]*True' \
  requirements.txt requirements-runtime.txt requirements-engines core engines data; then
  printf 'Runtime security policy rejected executable model-supplied Python.\n' >&2
  exit 1
fi

for requirement in \
  'transformers==5.5.0' \
  'diffusers==0.38.0' \
  'coqui-tts==0.27.5'; do
  contains_fixed "$requirement" requirements.txt requirements-runtime.txt requirements-engines \
    || { printf 'Required qualified dependency is missing: %s\n' "$requirement" >&2; exit 1; }
done

printf 'Runtime dependency security policy passed.\n'
