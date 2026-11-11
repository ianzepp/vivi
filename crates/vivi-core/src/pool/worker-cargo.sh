# Persistent Cargo output only. Loaded by the driver, and sourced by boot/job
# after checkout so rustup selects that repository's actual toolchain.
cargo_error() { echo "vivi-worker cargo: $*" >&2; return 1; }
cargo_request() { printf '%s\n' "$1" | sed -n "s/^$2=//p" | tail -n1; }
cargo_device() {
  local p=$1
  while [ ! -e "$p" ] && [ "$p" != / ]; do p=$(dirname "$p"); done
  df -Pk "$p" | awk 'NR == 2 { print $1 }'
}
cargo_env_line() { printf '%s=%q\n' "$1" "$2"; }

# Called under the worker lock, after the fresh slot archive is unpacked.
# A lifetime lock also protects this path from another mailspace/worker root
# using the same pool name. Never take over somebody else's target directory.
cargo_prepare() {
  [ "$(cargo_request "$2" cargo_enabled)" = 1 ] || return 0
  local id=$1 base pool index target entry cache cap device image_device nonce
  base=$(cargo_request "$2" cargo_root)
  [ -n "$base" ] || base=$(dirname "$(apfs_dir)")
  mkdir -p "$base" || return 1
  base=$(cd "$base" && pwd -P) || return 1
  case "$base/" in "$D/"*|"$(dir_root)/"*|"$R"/*/*/slot-*/*) cargo_error "root is inside a job boundary"; return 1 ;; esac
  device=$(cargo_device "$base"); image_device=$(cargo_device "$(apfs_dir)")
  [ -n "$device" ] && [ "$device" = "$image_device" ] \
    || { cargo_error "root must be on the images' filesystem"; return 1; }
  pool=${id#*/}; pool=${pool%/*}; index=${id##*/slot-}
  target="$base/slot-targets/$pool/slot-$(printf '%02d' "$index")"
  [ ! -L "$base/slot-targets" ] && [ ! -L "$base/slot-targets/$pool" ] && [ ! -L "$target" ] \
    || { cargo_error "target path is a symlink"; return 1; }
  mkdir -p "$(dirname "$target")" || return 1
  mkdir "$target.lock" 2>/dev/null \
    || { cargo_error "target is held by another slot: $target"; return 1; }
  nonce=$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')
  [ -n "$nonce" ] && printf '%s\n' "$nonce" > "$target.lock/owner" || return 1
  entry="$R/.ledger/$(flat "$id")"
  cap=$(cargo_request "$2" cargo_cap_gib)
  printf 'cargo_target=%s\ncargo_cap_kib=%s\ncargo_nonce=%s\n' "$target" "$((cap * 1048576))" "$nonce" >> "$entry" || return 1
  mkdir -p "$target" || return 1
  cargo_env_line CARGO_TARGET_DIR "$target" > "$D/build-cache.env" || return 1
  cargo_env_line VIVI_CARGO_SCCACHE "$(cargo_request "$2" cargo_sccache)" >> "$D/build-cache.env"
  if [ "$(cargo_request "$2" cargo_sccache)" = 1 ]; then
    cache="$base/sccache"
    [ ! -L "$cache" ] && [ ! -L "$cache/tmp" ] || return 1
    mkdir -p "$cache/tmp" || return 1
    {
      cargo_env_line RUSTC_WRAPPER sccache
      cargo_env_line SCCACHE_DIR "$cache"
      cargo_env_line SCCACHE_SERVER_UDS "$cache/server.sock"
      cargo_env_line CARGO_INCREMENTAL 0
    } >> "$D/build-cache.env"
  fi
}

# The boot/job wrapper is not tagged as an agent. The daemon has its own cwd,
# socket and TMPDIR outside all job boundaries, including on concurrent starts.
cargo_server() {
  (
    cd "$SCCACHE_DIR" || exit 1
    unset VIVI_AGENT_JOB VIVI_SLOT_ROOT WORK
    export TMPDIR="$SCCACHE_DIR/tmp"
    sccache --start-server || sccache --show-stats
  ) > /dev/null 2>&1
}

prepare_build_output() {
  [ -f "$W/build-cache.env" ] || return 0
  set -a; . "$W/build-cache.env"; set +a
  local version old
  version=$(cd "$1" && rustc -vV) || { cargo_error "cannot identify rustc"; return 1; }
  [ -n "$version" ] || return 1
  old=$(cat "$CARGO_TARGET_DIR/.vivi-rustc" 2>/dev/null)
  if [ "$version" != "$old" ]; then
    rm -rf -- "$CARGO_TARGET_DIR" || return 1
    mkdir -p "$CARGO_TARGET_DIR" || return 1
    printf '%s\n' "$version" > "$CARGO_TARGET_DIR/.vivi-rustc" || return 1
  fi
  [ "${VIVI_CARGO_SCCACHE:-0}" != 1 ] || cargo_server || { cargo_error "cannot start sccache"; return 1; }
}

# A retried old teardown must not touch a target already reserved anew.
cargo_owned() {
  local target nonce
  target=$(field "$1" cargo_target); nonce=$(field "$1" cargo_nonce)
  [ -n "$target" ] && [ -n "$nonce" ] && [ "$nonce" = "$(cat "$target.lock/owner" 2>/dev/null)" ]
}

# Called only after the slot's processes and boundary are gone, before its
# ledger entry is removed. An interrupted teardown can safely retry this.
cargo_trim() {
  local target cap size usage
  target=$(field "$1" cargo_target); cap=$(field "$1" cargo_cap_kib)
  [ -n "$target" ] && cargo_owned "$1" || return 0
  if [ -d "$target" ]; then
    usage=$(du -sk "$target") || return 1
    size=$(printf '%s\n' "$usage" | awk '{ print $1 }')
    [[ $size =~ ^[0-9]+$ ]] || return 1
    if [ "$size" -gt "$cap" ]; then rm -rf -- "$target" || return 1; fi
  fi
  rm -rf -- "$target.lock"
}
