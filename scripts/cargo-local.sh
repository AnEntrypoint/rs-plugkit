#!/usr/bin/env sh
# Runs cargo with a writable CARGO_HOME. A root-owned or read-only CARGO_HOME
# (e.g. /config/.cargo in the gm container) cannot hold git dependency caches,
# so fall back to a per-user directory under /tmp.
set -eu
home="${CARGO_HOME:-$HOME/.cargo}"
if [ ! -d "$home" ] || [ ! -w "$home" ]; then
  home="/tmp/cargo-home-$(id -u)"
  mkdir -p "$home"
fi
CARGO_HOME="$home" exec cargo "$@"
