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
contains_fixed 'if [[ "$ENGINE" == "breeze" || "$ENGINE" == "fish-speech" ]]; then' setup-engine-runtime.sh \
  || { printf 'Qualified compatibility runtimes must remain standalone.\n' >&2; exit 1; }
contains_fixed 'BREEZE_SOURCE_REVISION="ca632ce6c4d05f7985da4eab29b1a5d445b43f7b"' setup-engine-runtime.sh \
  || { printf 'The qualified Breeze source revision changed.\n' >&2; exit 1; }
contains_fixed 'FISH_SOURCE_REVISION="58046eaa1a4cefb0c8cc3a3a667b34186ea02dde"' setup-engine-runtime.sh \
  || { printf 'The qualified Fish Speech source revision changed.\n' >&2; exit 1; }
contains_fixed 'local_files_only=True' engines/tts/breeze_tts.py \
  || { printf 'Breeze must continue loading only verified local model assets.\n' >&2; exit 1; }

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
