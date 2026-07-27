#!/usr/bin/env bash
set -euo pipefail

WEIGHTS_DIR="$(cd "$(dirname "$0")/.." && pwd)/weights"
mkdir -p "$WEIGHTS_DIR"

MODEL_URL="https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/main/model.safetensors"
MODEL_PATH="$WEIGHTS_DIR/bge-small-en-v1.5.safetensors"
MODEL_SHA="3c9f31665447c8911517620762200d2245a2518d6e7208acc78cd9db317e21ad"

TOK_URL="https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/main/tokenizer.json"
TOK_PATH="$WEIGHTS_DIR/bge-tokenizer.json"
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
  # Retry with exponential backoff. A single bare curl made every release
  # hostage to upstream rate limiting: huggingface.co returned 429 on a real
  # release run whose only change was unrelated, failing the whole workflow
  # while Build and Deploy passed on the identical commit.
  #
  # Retrying is safe here precisely BECAUSE the sha256 check below is
  # independent of the transport -- a truncated or rate-limit-error-body
  # response fails verification and is retried rather than trusted, so this
  # widens the window for a good download without weakening integrity.
  local attempt=1 max_attempts=5 delay=5
  while true; do
    if curl -L -f --retry 3 --retry-delay 2 --retry-connrefused -o "$path" "$url"; then
      break
    fi
    if [ "$attempt" -ge "$max_attempts" ]; then
      echo "failed to fetch $url after $max_attempts attempts" >&2
      exit 1
    fi
    echo "fetch attempt $attempt/$max_attempts failed; retrying in ${delay}s" >&2
    sleep "$delay"
    attempt=$((attempt + 1))
    delay=$((delay * 2))
  done
  if ! check_sha "$path" "$expected"; then
    echo "sha mismatch for $path" >&2
    exit 1
  fi
}

fetch "$MODEL_URL" "$MODEL_PATH" "$MODEL_SHA"
fetch "$TOK_URL" "$TOK_PATH" "$TOK_SHA"

echo "weights ready in $WEIGHTS_DIR (model: $(stat -c%s "$MODEL_PATH" 2>/dev/null || stat -f%z "$MODEL_PATH") bytes)"
