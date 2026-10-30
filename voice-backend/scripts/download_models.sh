#!/usr/bin/env bash
# Downloads the ONNX models into models/ (git-ignored). Safe to re-run:
# existing files are kept. Override any URL with an environment variable.
set -euo pipefail

cd "$(dirname "$0")/.."
MODELS_DIR="${MODELS_DIR:-models}"

SILERO_URL="${SILERO_URL:-https://github.com/snakers4/silero-vad/raw/v5.1.2/src/silero_vad/data/silero_vad.onnx}"
EMBED_REPO="${EMBED_REPO:-https://huggingface.co/Xenova/bge-m3/resolve/main}"
EMBED_MODEL_URL="${EMBED_MODEL_URL:-$EMBED_REPO/onnx/model_quantized.onnx}"
EMBED_TOKENIZER_URL="${EMBED_TOKENIZER_URL:-$EMBED_REPO/tokenizer.json}"

fetch() {
  local url="$1" dest="$2"
  if [[ -s "$dest" ]]; then
    echo "ok      $dest (already present)"
    return
  fi
  echo "fetch   $url"
  mkdir -p "$(dirname "$dest")"
  curl -fL --retry 3 --progress-bar -o "$dest.part" "$url"
  mv "$dest.part" "$dest"
}

fetch "$SILERO_URL" "$MODELS_DIR/silero_vad.onnx"
fetch "$EMBED_MODEL_URL" "$MODELS_DIR/bge-m3/model_quantized.onnx"
fetch "$EMBED_TOKENIZER_URL" "$MODELS_DIR/bge-m3/tokenizer.json"

echo
echo "sha256:"
sha256sum "$MODELS_DIR/silero_vad.onnx" "$MODELS_DIR"/bge-m3/*
