#!/bin/bash
# Build the Linux vivi binary for the lab from this checkout, in a Rust
# container. The cargo registry and the build output live on named volumes, so
# a rebuild after a source change is incremental. Output: lab/bin/vivi.
# OUT=<name> writes lab/bin/<name> instead.
set -euo pipefail
. "$(dirname "$0")/env.sh"
SRC=$(cd "$LAB/.." && pwd)
# One target volume per source tree: every tree is mounted at /src and cargo
# judges freshness by mtime, so two trees sharing one target can link a stale
# crate of the other tree.
VOL=vivi-lab-target-$(printf '%s' "$SRC" | shasum | cut -c1-12)
docker run --rm \
  -v "$SRC":/src:ro \
  -v vivi-lab-cargo:/usr/local/cargo/registry \
  -v "$VOL":/target \
  -v "$LAB/bin":/out \
  -e CARGO_TARGET_DIR=/target \
  -w /src \
  rust:1.93-bookworm \
  bash -c 'cargo build --release --locked --bin vivi && cp /target/release/vivi /out/'"${OUT:-vivi}"
ls -la "$LAB/bin/${OUT:-vivi}"
