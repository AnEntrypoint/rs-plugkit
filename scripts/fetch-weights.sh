#!/usr/bin/env bash
set -euo pipefail

WEIGHTS_DIR="$(cd "$(dirname "$0")/.." && pwd)/weights"
mkdir -p "$WEIGHTS_DIR"

MODEL_URL="https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/model.safetensors"
MODEL_PATH="$WEIGHTS_DIR/minilm-l6-v2.safetensors"
MODEL_SHA="53aa51172d142c89d9012cce15ae4d6cc0ca6895895114379cacb4fab128d9db"

TOK_URL="https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json"
TOK_PATH="$WEIGHTS_DIR/tokenizer.json"
TOK_SHA="be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037"

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

fetch "$MODEL_URL" "$MODEL_PATH" "$MODEL_SHA"
fetch "$TOK_URL" "$TOK_PATH" "$TOK_SHA"
echo "weights ready in $WEIGHTS_DIR"
