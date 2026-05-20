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

F32_PATH="$WEIGHTS_DIR/minilm-l6-v2.f32.safetensors"
F32_SHA="$MODEL_SHA"
F16_PATH="$MODEL_PATH"

fetch "$MODEL_URL" "$F32_PATH" "$F32_SHA"
fetch "$TOK_URL" "$TOK_PATH" "$TOK_SHA"

if [ ! -s "$F16_PATH" ] || [ "$F32_PATH" -nt "$F16_PATH" ]; then
  echo "converting $F32_PATH (F32) -> $F16_PATH (F16)"
  pip install --quiet safetensors torch --index-url https://download.pytorch.org/whl/cpu || pip install --quiet safetensors torch
  python3 - "$F32_PATH" "$F16_PATH" <<'PY'
import sys
from safetensors.torch import load_file, save_file
src, dst = sys.argv[1], sys.argv[2]
tensors = load_file(src)
converted = {k: v.to('cpu').half() for k, v in tensors.items()}
save_file(converted, dst)
print(f"wrote {dst}")
PY
fi

echo "weights ready in $WEIGHTS_DIR (F16: $(stat -c%s "$F16_PATH" 2>/dev/null || stat -f%z "$F16_PATH") bytes)"
