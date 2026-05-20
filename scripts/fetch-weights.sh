#!/usr/bin/env bash
set -euo pipefail

WEIGHTS_DIR="$(cd "$(dirname "$0")/.." && pwd)/weights"
mkdir -p "$WEIGHTS_DIR"

GGUF_URL="https://huggingface.co/nomic-ai/nomic-embed-text-v1.5-GGUF/resolve/main/nomic-embed-text-v1.5.Q4_K_M.gguf"
GGUF_PATH="$WEIGHTS_DIR/nomic-q4.gguf"
GGUF_SHA="d4e388894e09cf3816e8b0896d81d265b55e7a9fff9ab03fe8bf4ef5e11295ac"

TOK_URL="https://huggingface.co/nomic-ai/nomic-embed-text-v1.5/resolve/main/tokenizer.json"
TOK_PATH="$WEIGHTS_DIR/tokenizer.json"
TOK_SHA="d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66"

check_sha() {
  local path="$1" expected="$2"
  [ -f "$path" ] || return 1
  local actual
  actual=$(sha256sum "$path" | awk '{print $1}')
  [ "$actual" = "$expected" ]
}

fetch() {
  local url="$1" path="$2" expected="$3"
  if check_sha "$path" "$expected"; then
    echo "ok: $path (sha matches)"
    return 0
  fi
  echo "fetching $url -> $path"
  curl -L -f -o "$path" "$url"
  if ! check_sha "$path" "$expected"; then
    echo "sha mismatch for $path" >&2
    exit 1
  fi
}

fetch "$GGUF_URL" "$GGUF_PATH" "$GGUF_SHA"
fetch "$TOK_URL" "$TOK_PATH" "$TOK_SHA"
echo "weights ready in $WEIGHTS_DIR"
