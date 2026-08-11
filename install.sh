#!/bin/bash
set -e

python3 -m venv .venv
source .venv/bin/activate

python3 -m pip install --upgrade pip
python3 -m pip install torch torchaudio --index-url https://download.pytorch.org/whl/cu124
python3 -m pip install -r requirements.txt

mkdir -p ~/.soundAr/models
mkdir -p ~/.soundAr/state

echo "Installation complete. Run: source .venv/bin/activate && python3 main.py"
