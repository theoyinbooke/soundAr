#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if rg -n \
  'transformers==4\.|transformers[><=]+4\.|diffusers==0\.29|TTS==0\.22|trust_remote_code[[:space:]]*=[[:space:]]*True' \
  requirements.txt requirements-runtime.txt requirements-engines; then
  printf 'Runtime security policy rejected a vulnerable framework pin or executable model code.\n' >&2
  exit 1
fi

if rg -n 'trust_remote_code[[:space:]]*=[[:space:]]*True' core engines data; then
  printf 'Runtime security policy rejected executable model-supplied Python.\n' >&2
  exit 1
fi

for requirement in \
  'transformers==5.5.0' \
  'diffusers==0.38.0' \
  'coqui-tts==0.27.5'; do
  rg -q --fixed-strings "$requirement" requirements.txt requirements-runtime.txt requirements-engines \
    || { printf 'Required qualified dependency is missing: %s\n' "$requirement" >&2; exit 1; }
done

printf 'Runtime dependency security policy passed.\n'
