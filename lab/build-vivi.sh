#!/bin/bash
# Build the Linux vivi binary for the lab from this checkout, in a Rust
# container. The cargo registry and the build output live on named volumes, so
# a rebuild after a source change is incremental. Output: lab/bin/vivi.
set -euo pipefail
. "$(dirname "$0")/env.sh"
SRC=$(cd "$LAB/.." && pwd)
docker run --rm \
  -v "$SRC":/src:ro \
  -v vivi-lab-cargo:/usr/local/cargo/registry \
  -v vivi-lab-target:/target \
  -v "$LAB/bin":/out \
  -e CARGO_TARGET_DIR=/target \
  -w /src \
  rust:1.93-bookworm \
  bash -c 'cargo build --release --locked --bin vivi && cp /target/release/vivi /out/vivi'
ls -la "$LAB/bin/vivi"
