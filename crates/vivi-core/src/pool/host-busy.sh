# Host-wide admission, shared by SSH and bounded-worker slots. Perl flock is
# supported on both Linux and macOS. The lock is held by a parent process,
# never inherited by a detached job; a killed claimant releases it in-kernel.
busy_root() { printf '%s' "${VIVI_WORKER_ROOT:-$HOME/vivi-worker}"; }
busy_limit() { sed -n 's/^max_busy[[:space:]]*=[[:space:]]*//p' "$(busy_root)/worker.conf" 2>/dev/null | tail -n1; }
busy_lock() {
  [ "$VERB" = claim ] && [ -z "${VIVI_BUSY_LOCKED:-}" ] || return 0
  local limit root
  limit=$(busy_limit)
  [ -n "$limit" ] || return 0
  [[ $limit =~ ^[1-9][0-9]*$ ]] || fail "worker.conf max_busy must be a positive integer"
  root=$(busy_root)
  mkdir -p "$root/.busy" || fail "cannot create host admission directory"
  export VIVI_BUSY_LOCKED=1
  exec perl -MFcntl=:flock -e 'open(my $l, ">>", shift) or die $!; flock($l, LOCK_EX) or die $!; system @ARGV; exit($? == -1 ? 1 : $? >> 8)' \
    "$root/.busy.lock" bash -c "$BASH_EXECUTION_STRING"
}

# A reservation has a small guardian process, covering the delivery window as
# well as the job. It exits after a dead boot or abandoned (120s) delivery.
# The token is also removed by the boot script immediately after job end.
busy_take() {
  local limit root token n pid nonce_value attempt ready
  limit=$(busy_limit); [ -n "$limit" ] || return 0
  root=$(busy_root)
  for token in "$root"/.busy/*; do
    [ -f "$token" ] || continue
    read -r pid nonce_value < "$token"
    # Verify identity, not merely kill -0 (which accepts reused PIDs).
    if ! ps -ww -E -o command= -p "$pid" 2>/dev/null | tr ' \t' '\n\n' | grep -qxF "VIVI_BUSY_TOKEN=$nonce_value"; then
      # Linux ps uses e, macOS -E. /proc is also exact and avoids ps variants.
      if [ ! -r "/proc/$pid/environ" ] || ! tr '\0' '\n' < "/proc/$pid/environ" | grep -qxF "VIVI_BUSY_TOKEN=$nonce_value"; then rm -f "$token"; fi
    fi
  done
  n=$(find "$root/.busy" -type f | wc -l)
  [ "$n" -lt "$limit" ] || return 1
  token="$root/.busy/$nonce"
    printf '0 %s\n' "$nonce" > "$token"
    # The flock serialises token selection and complete record publication.
    export -f alive
    export SYSTEM
    env VIVI_BUSY_TOKEN="$nonce" nohup bash -c '
      token=$1; dir=$2; nonce=$3; seen=0
      : > "$dir/busy-ready.$nonce"
      for ((i=0; ; i++)); do
        if [ -f "$token" ]; then
          read -r holder value < "$token"
          [ "$value" = "$nonce" ] || exit 0
          [ -f "$dir/claim" ] || break
          if [ -s "$dir/boot.pid" ]; then
            p=$(cat "$dir/boot.pid")
            if alive "$dir"; then seen=1; else break; fi
          elif [ "$seen" = 1 ] || [ "$i" -ge 1200 ]; then break; fi
        elif [ "$i" -gt 10 ]; then exit 0; fi
        sleep 0.1
      done
      [ "$(awk "{print \$2}" "$token" 2>/dev/null)" != "$nonce" ] || rm -f "$token"
    ' bash "$token" "$D" "$nonce" >/dev/null 2>&1 </dev/null &
    pid=$!
    ready="$D/busy-ready.$nonce"
    for ((attempt=0; attempt<200; attempt++)); do
      [ -f "$ready" ] && break
      sleep 0.01
    done
    if [ ! -f "$ready" ]; then kill "$pid" 2>/dev/null; rm -f "$token"; return 1; fi
    rm -f "$ready"
    printf '%s %s\n' "$pid" "$nonce" > "$token"
    printf '%s\n' "$token" > "$D/busy-token"
  return 0
}

busy_release() {
  local token value held
  token=$(cat "$D/busy-token" 2>/dev/null) || return 0
  held=$(sed -n 's/^nonce=//p' "$D/claim" 2>/dev/null)
  value=$(awk '{print $2}' "$token" 2>/dev/null)
  [ -z "$held" ] || [ "$held" != "$value" ] || rm -f "$token"
  rm -f "$D/busy-token"
}
