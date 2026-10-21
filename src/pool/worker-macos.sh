# Vivi pool: the remote half of the `worker` provider on macOS. Not run on
# its own: the provider sends this text, after assignments of ROOT, VERB,
# UMASK and A1..A4, as the script of one `bash -c` on the worker, once per
# call, so the worker never holds a stale copy. Works with the stock bash 3.2.
#
# It answers the same verbs as `ssh-slot.sh`, so the provider reuses the ssh
# provider's calls and parsing. The difference is the slot: every slot is a
# bounded workspace made for one job (`create`) and torn down after it
# (`destroy`), and a slot's processes are whatever uses that workspace.
# Design: docs/worker-driver-protocol.md.
#
# Boundary kinds, named by the request `kind=<k> size_gib=<n>` (A2 of create).
# All three work over SSH with nobody logged in at the console:
#   apfs  a sparse APFS disk image of size_gib under apfs_dir, made by the
#         login user (so its volume is the user's), attached and mounted at
#         the slot path, detached and deleted at the end. The image's size is
#         a hard limit; it takes disk only as it is written. (A volume added
#         to a container instead mounts writable only while someone is logged
#         in at the console.)
#   ram   a RAM disk of size_gib (hdiutil + diskutil), freed by eject
#   dir   a plain directory under dir_root, no limit
#
# Layout under $R (ROOT, `~/` expanded; keep it on the internal disk: macOS
# will not mount a volume inside another external volume):
#   worker.conf                 operator budgets, key=value (conf below)
#   .lock/                      held while a slot is made or torn down
#   .ledger/<flat id>           kind, size and what was built for a slot
#   <mailspace>/<pool>/slot-<n> the slot: a mount point, or a link to its
#                               dir_root directory. Inside, as for ssh:
#                               marks/, created, claim, boot.pid, boot.start,
#                               exit_code, slot.env, boot.sh and its files.
#
# Verbs: list, machine, probe, envgaps, create, start, stop, claim, unclaim,
# put, seal, read, exec, destroy (as in ssh-slot.sh), and offer.
umask "$UMASK"
fail() { echo "vivi-worker: $*" >&2; exit 1; }
refuse() { echo "vivi-worker: refusing to delete: $*" >&2; exit 92; }

case "$ROOT" in
  "~/"*) R="$HOME/${ROOT#\~/}" ;;
  *) R=$ROOT ;;
esac

slot_ok() { [[ $1 =~ ^[a-z0-9][a-z0-9-]*/[a-z0-9][a-z0-9-]*/slot-[0-9]+$ ]]; }
need_slot() {
  slot_ok "$1" || fail "not a slot id: $1"
  D=$(cd "$R/$1" 2>/dev/null && pwd -P) || D="$R/$1"
}
known() { [ -f "$D/marks/vivi_pool" ]; }
flat() { printf '%s' "${1//\//-}"; }

# One operator setting from worker.conf, else the default $2.
conf() {
  local v
  v=$(sed -n "s/^$1=//p" "$R/worker.conf" 2>/dev/null | tail -n1)
  printf '%s' "${v:-$2}"
}
home_path() { case "$1" in "~/"*) printf '%s' "$HOME/${1#\~/}" ;; *) printf '%s' "$1" ;; esac; }
dir_root() { home_path "$(conf dir_root "$R/.dirs")"; }
apfs_dir() { home_path "$(conf apfs_dir "$R/.images")"; }
# One field of slot $1's ledger entry.
field() { sed -n "s/^$2=//p" "$R/.ledger/$(flat "$1")" 2>/dev/null | tail -n1; }

lock() {
  local _
  mkdir -p "$R"
  find "$R/.lock" -maxdepth 0 -mmin +10 -exec rmdir {} \; 2>/dev/null
  for _ in $(seq 300); do
    if mkdir "$R/.lock" 2>/dev/null; then
      trap 'rmdir "$R/.lock" 2>/dev/null' EXIT
      return 0
    fi
    sleep 0.1
  done
  fail "the worker lock is busy"
}

# --- capacity -------------------------------------------------------------

reserved() {
  local f sum=0 size
  for f in "$R"/.ledger/*; do
    [ -f "$f" ] || continue
    [ "$(sed -n 's/^kind=//p' "$f")" = "$1" ] || continue
    size=$(sed -n 's/^size_gib=//p' "$f")
    sum=$((sum + ${size:-0}))
  done
  echo "$sum"
}
mem_free_gib() {
  vm_stat | awk '
    /page size of/ { ps = $8 }
    /^Pages (free|inactive|speculative):/ { gsub(/\./, "", $NF); n += $NF }
    END { printf "%d\n", n * ps / 1073741824 }'
}
# Free GiB on the file system that holds path $1 (or its nearest parent).
disk_free_gib() {
  local p=$1
  while [ ! -e "$p" ] && [ "$p" != / ]; do p=$(dirname "$p"); done
  df -Pk "$p" 2>/dev/null | awk 'NR == 2 { printf "%d\n", $4 / 1048576 }'
}
offer() {
  local t tools=
  echo "system=$(uname -s) $(uname -m)"
  echo "kinds=ram apfs dir"
  echo "ram_budget_gib=$(conf ram_budget_gib 0) ram_reserved_gib=$(reserved ram) mem_free_gib=$(mem_free_gib)"
  echo "apfs_dir=$(apfs_dir) apfs_budget_gib=$(conf apfs_budget_gib 0) apfs_reserved_gib=$(reserved apfs) apfs_free_gib=$(disk_free_gib "$(apfs_dir)")"
  [ "$(uname -m)" = arm64 ] && echo "gpu=metal" || echo "gpu=none"
  for t in git cargo node python3 perl; do
    command -v "$t" > /dev/null 2>&1 && tools="$tools $t"
  done
  echo "tools=${tools# }"
}

# Refuse a request this worker cannot hold now. The words "no free slot" let
# the pump count the pool as full and try the next one.
undeliverable() { fail "no free slot: $*"; }
admit_check() {
  local kind=$1 size=$2 budget used free margin
  case "$kind" in
    ram)
      budget=$(conf ram_budget_gib 0); used=$(reserved ram)
      margin=$(conf ram_margin_gib 8); free=$(mem_free_gib)
      [ $((used + size)) -le "$budget" ] \
        || undeliverable "ram ${size}g exceeds the budget (${used}g of ${budget}g reserved)"
      [ $((size + margin)) -le "$free" ] \
        || undeliverable "ram ${size}g plus a ${margin}g margin exceeds ${free}g of free memory"
      ;;
    apfs)
      budget=$(conf apfs_budget_gib 0); used=$(reserved apfs)
      free=$(disk_free_gib "$(apfs_dir)")
      [ -d "$(apfs_dir)" ] || undeliverable "apfs_dir $(apfs_dir) is not there (is its drive mounted?)"
      [ $((used + size)) -le "$budget" ] \
        || undeliverable "apfs ${size}g exceeds the budget (${used}g of ${budget}g reserved)"
      [ "$size" -le "${free:-0}" ] \
        || undeliverable "apfs ${size}g exceeds ${free:-0}g free under $(apfs_dir)"
      ;;
    dir) ;;
    *) fail "this worker offers no boundary of kind $kind" ;;
  esac
}

# --- boundaries -----------------------------------------------------------

# Mount the APFS volume of an attached disk at $2; its device is $1.
mount_at() {
  diskutil mount -mountPoint "$2" "$1" > /dev/null || return 1
  : > "$2/.metadata_never_index"
}
build_ram() {
  local name=$1 at=$2 size=$3 disk volume
  disk=$(hdiutil attach -nomount "ram://$((size * 2097152))" 2>/dev/null | awk 'NR == 1 { print $1 }')
  [ -n "$disk" ] || return 1
  echo "device=$disk"
  diskutil eraseVolume APFS "$name" "$disk" > /dev/null || return 1
  volume=$(diskutil info -plist "/Volumes/$name" | plutil -extract DeviceIdentifier raw -o - - 2>/dev/null)
  [ -n "$volume" ] || return 1
  diskutil unmount "$volume" > /dev/null || return 1
  mount_at "$volume" "$at"
}
build_apfs() {
  local name=$1 at=$2 size=$3 img out disk volume
  img="$(apfs_dir)/$name.sparseimage"
  mkdir -p "$(apfs_dir)" || return 1
  echo "image=$img"
  hdiutil create -size "${size}g" -type SPARSE -fs APFS -volname "$name" -quiet "$img" 2>/dev/null || return 1
  out=$(hdiutil attach -nomount "$img" 2>/dev/null) || return 1
  disk=$(printf '%s\n' "$out" | awk 'NR == 1 { print $1 }')
  # The APFS volume line carries the APFS volume type GUID.
  volume=$(printf '%s\n' "$out" | awk '/41504653-0000-11AA-AA11/ { print $1 }' | tail -n1)
  echo "device=$disk"
  [ -n "$volume" ] || return 1
  mount_at "$volume" "$at"
}

# Make the boundary of slot $1 for request $2 at the slot path.
make_boundary() {
  local id=$1 kind size name entry at rc dir
  kind=$(printf '%s\n' $2 | sed -n 's/^kind=//p')
  size=$(printf '%s\n' $2 | sed -n 's/^size_gib=//p')
  [[ ${size:-0} =~ ^[0-9]+$ ]] || fail "size_gib is not a whole number: $size"
  size=${size:-0}
  admit_check "$kind" "$size"
  name="vivi-$(flat "$id")"
  at="$R/$id"
  mkdir -p "$R/.ledger" "$(dirname "$at")" || fail "cannot make $R"
  entry="$R/.ledger/$(flat "$id")"
  # The entry is written before anything is built, so a failure part way
  # leaves enough for teardown to clean up after.
  printf 'kind=%s\nsize_gib=%s\nmount=%s\nname=%s\n' "$kind" "$size" "$at" "$name" > "$entry"
  case "$kind" in
    ram) mkdir -p "$at" && build_ram "$name" "$at" "$size" >> "$entry"; rc=$? ;;
    apfs) mkdir -p "$at" && build_apfs "$name" "$at" "$size" >> "$entry"; rc=$? ;;
    dir)
      dir="$(dir_root)/$(flat "$id")"
      echo "dir=$dir" >> "$entry"
      mkdir -p "$dir" && ln -s "$dir" "$at"; rc=$?
      ;;
  esac
  if [ "$rc" -ne 0 ]; then
    teardown "$id"
    fail "could not build the $kind boundary of $id"
  fi
}

# The pids with a file, or their working directory, on slot $1's boundary,
# plus the boot wrapper's process group and every descendant of it.
users_of() {
  local id=$1 kind at p
  kind=$(field "$id" kind); at=$(field "$id" mount)
  [ "$kind" != dir ] || at=$(field "$id" dir)
  {
    if [ -n "$at" ] && [ -d "$at" ]; then
      if [ "$kind" = dir ]; then lsof -t +D "$at" 2>/dev/null; else lsof -t +f -- "$at" 2>/dev/null; fi
    fi
    p=$(cat "$R/$id/boot.pid" 2>/dev/null)
    if [[ $p =~ ^[0-9]+$ ]] && alive "$R/$id"; then
      ps -axo pid=,ppid=,pgid= 2>/dev/null | awk -v b="$p" '
        { pid[NR] = $1; pp[NR] = $2; pg[NR] = $3 }
        END {
          mark[b] = 1
          for (i in pid) if (pg[i] == b) mark[pid[i]] = 1
          for (more = 1; more; ) {
            more = 0
            for (i in pid) if (!(pid[i] in mark) && (pp[i] in mark)) { mark[pid[i]] = 1; more = 1 }
          }
          for (q in mark) print q
        }'
    fi
  } | sort -un | grep -v -x "$$"
}

kill_users() {
  local id=$1 sig pids _
  for sig in TERM KILL; do
    pids=$(users_of "$id" | tr '\n' ' ')
    [ -n "${pids// /}" ] || return 0
    kill -"$sig" $pids 2>/dev/null
    for _ in $(seq 50); do
      [ -n "$(users_of "$id")" ] || return 0
      sleep 0.1
    done
  done
  [ -z "$(users_of "$id")" ]
}

# Stop everything on slot $1's boundary, then remove the boundary and its
# ledger entry. Only what the ledger recorded is touched.
teardown() {
  local id=$1 kind at device image dir
  [ -f "$R/.ledger/$(flat "$id")" ] || return 0
  kind=$(field "$id" kind); at=$(field "$id" mount)
  device=$(field "$id" device); image=$(field "$id" image); dir=$(field "$id" dir)
  kill_users "$id" || fail "could not stop the processes of $id"
  case "$kind" in
    ram) [ -z "$device" ] || diskutil eject "$device" > /dev/null 2>&1 ;;
    apfs)
      [ -z "$device" ] || hdiutil detach "$device" -force -quiet 2>/dev/null
      case "$image" in "$(apfs_dir)"/vivi-*.sparseimage) rm -f -- "$image" ;; esac
      ;;
    dir) case "$dir" in "$(dir_root)"/*) rm -rf -- "$dir" ;; esac ;;
  esac
  # A path still mounted is never removed (its contents are not ours).
  if ! mount | grep -q " on $at ("; then
    case "$at" in "$R"/*/*/slot-*) rm -rf -- "$at" ;; esac
  fi
  rm -f -- "$R/.ledger/$(flat "$id")"
  rmdir "$(dirname "$at")" "$(dirname "$(dirname "$at")")" 2>/dev/null
  return 0
}

# --- the slot verbs (as in ssh-slot.sh) -----------------------------------

# The boot wrapper of slot dir $1 is running: its pid is alive with the start
# time recorded when it began (a reused pid has another; an empty time from a
# boot that ended before it was read never matches).
alive() {
  local p now
  p=$(cat "$1/boot.pid" 2>/dev/null) || return 1
  [[ $p =~ ^[0-9]+$ ]] || return 1
  now=$(ps -o lstart= -p "$p" 2>/dev/null)
  [ -n "$now" ] && [ "$now" = "$(cat "$1/boot.start" 2>/dev/null)" ]
}

emit() {
  local id=$1 d f state=stopped
  d=$(cd "$R/$id" && pwd -P) || return 0
  echo "@@slot $id"
  alive "$d" && state=started
  echo "state=$state"
  [ "$state" = stopped ] && [ -f "$d/exit_code" ] && echo "exit=$(head -n1 "$d/exit_code")"
  echo "created=$(head -n1 "$d/created" 2>/dev/null)"
  for f in "$d"/marks/*; do
    [ -f "$f" ] && printf 'mark.%s=%s\n' "${f##*/}" "$(head -n1 "$f")"
  done
  if [ -f "$d/claim" ]; then
    sed -n 's/^task=/claim.task=/p; s/^claimed=/claim.claimed=/p' "$d/claim"
  fi
  return 0
}

list_slots() {
  local m id
  [ -d "$R" ] || return 0
  # No pool (A2) lists every pool of the mailspace: a worker provider serves
  # its pool and that pool's -doctor test pool alike.
  for m in "$R"/"$A1"/${A2:-*}/slot-*/marks/vivi_pool; do
    [ -f "$m" ] || continue
    id=${m#"$R"/}; id=${id%/marks/vivi_pool}
    slot_ok "$id" && emit "$id"
  done
  return 0
}

create_slot() {
  slot_ok "$A1" || fail "not a slot id: $A1"
  [ -e "$R/$A1" ] && fail "slot exists: $A1"
  lock
  make_boundary "$A1" "$A2"
  need_slot "$A1"
  tar -xf - -C "$D" || { teardown "$A1"; fail "could not unpack the new slot"; }
  date -u +%Y-%m-%dT%H:%M:%SZ > "$D/created"
  [ ! -f "$D/once.env" ] || chmod 600 "$D/once.env"
  [ ! -f "$D/slot.env" ] || chmod 600 "$D/slot.env"
  # Stand-ins for GNU tools macOS lacks; they come last on PATH.
  [ ! -d "$D/bin" ] || chmod 700 "$D"/bin/*
  uname -m | sed 's/x86_64/amd64/; s/aarch64/arm64/' > "$D/marks/vivi_pool_arch"
}

start_slot() {
  local _
  need_slot "$A1"; known || fail "no such slot: $A1"
  alive "$D" && return 0
  rm -f "$D/exit_code" "$D/boot.pid" "$D/boot.start"
  (
    set -a; . "$D/slot.env"; set +a
    export WORK="$D" VIVI_SLOT_ROOT="$D"
    cd "$D" || exit 1
    nohup perl -MPOSIX -e 'POSIX::setsid() or die "setsid: $!\n"; exec @ARGV or die "exec: $!\n"' \
      bash -l -c 'echo $$ > "$WORK/boot.pid"; bash "$WORK/boot.sh"; rc=$?; echo "$rc" > "$WORK/exit_code"; exit "$rc"' \
      > "$D/boot.out" 2>&1 < /dev/null &
  )
  for _ in $(seq 100); do
    if [ -s "$D/boot.pid" ]; then
      ps -o lstart= -p "$(cat "$D/boot.pid")" > "$D/boot.start" 2>/dev/null
      return 0
    fi
    sleep 0.05
  done
  fail "the slot did not start (see boot.out in $A1)"
}

stop_slot() {
  need_slot "$A1"; [ -d "$D" ] || return 0
  kill_users "$A1" || fail "could not stop the processes of $A1"
  rm -f "$D/boot.pid" "$D/boot.start" "$D/exit_code"
}

free_now() {
  [ "$(head -n1 "$D/marks/vivi_pool_state" 2>/dev/null)" = ready ] && ! alive "$D"
}

claim_slot() {
  local nonce tmp
  need_slot "$A1"; known || { echo gone; return 0; }
  nonce=$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')
  tmp="$D/claim.$$.$nonce.tmp"
  printf 'task=%s\nclaimed=%s\nnonce=%s\n' "$A2" "$A3" "$nonce" > "$tmp" || fail "cannot write the claim"
  if ln "$tmp" "$D/claim" 2>/dev/null; then
    rm -f "$tmp"
    if free_now; then echo "claimed $nonce"; else rm -f "$D/claim"; echo busy; fi
  else
    rm -f "$tmp"
    [ -e "$D/claim" ] && echo taken || fail "cannot create the claim (hard links unavailable?)"
  fi
}

put_file() {
  local held
  need_slot "$A1"; known || fail "no such slot: $A1"
  held=$(sed -n 's/^nonce=//p' "$D/claim" 2>/dev/null)
  [ -n "$held" ] && [ "$held" = "$A3" ] || fail "not the claim holder of $A1"
  case "$A2" in ''|/*|*..*) fail "bad path: $A2" ;; esac
  mkdir -p "$(dirname "$D/$A2")" && ( umask 077; cat > "$D/$A2.partial" ) && mv -f "$D/$A2.partial" "$D/$A2" \
    || fail "cannot write $A2"
}

seal_mark() {
  need_slot "$A1"; known || fail "no such slot: $A1"
  [[ $A2 =~ ^[a-z_]+$ ]] || fail "bad mark name: $A2"
  printf '%s\n' "$A3" > "$D/marks/$A2.partial" && mv -f "$D/marks/$A2.partial" "$D/marks/$A2" \
    || fail "cannot write mark $A2"
}

read_files() {
  local p list=
  need_slot "$A1"; cd "$D" 2>/dev/null || return 0
  for p in $A2; do
    case "$p" in ''|/*|*..*) continue ;; esac
    [ -f "$p" ] && list="$list $p"
  done
  [ -z "$list" ] || tar -cf - $list
}

# Deleting is the one dangerous act: the id is a slot id, the root is a real
# directory other than / and the home, and the slot holds Vivi's own marks
# naming this pool and this mailspace (or is only a ledger entry left by a
# create that failed part way).
destroy_slot() {
  local pool space root
  slot_ok "$A1" || refuse "not a slot id: $A1"
  case "$R" in ''|/) refuse "the root is empty or /" ;; /*) ;; *) refuse "the root is not absolute: $R" ;; esac
  root=$(cd "$R" 2>/dev/null && pwd -P) || { echo gone; return 0; }
  case "$root" in /|"$(cd "$HOME" 2>/dev/null && pwd -P)") refuse "the root is / or the home directory: $root" ;; esac
  lock
  if [ ! -e "$R/$A1" ] && [ ! -f "$R/.ledger/$(flat "$A1")" ]; then echo gone; return 0; fi
  if [ -e "$R/$A1/marks" ]; then
    space=${A1%%/*}; pool=${A1#*/}; pool=${pool%/*}
    [ "$(head -n1 "$R/$A1/marks/vivi_pool" 2>/dev/null)" = "$pool" ] || refuse "$A1 carries no Vivi mark for pool $pool"
    [ "$(head -n1 "$R/$A1/marks/vivi_pool_mailspace" 2>/dev/null)" = "$space" ] || refuse "$A1 carries no Vivi mark for mailspace $space"
  fi
  teardown "$A1"
  echo destroyed
}

probe() {
  local t anchor
  echo "user=$(id -un)"
  echo "host=$(hostname 2>/dev/null)"
  echo "system=$(uname -s) $(uname -m)"
  for t in git tar ps od awk sed perl lsof diskutil hdiutil plutil; do
    command -v "$t" > /dev/null 2>&1 || echo "missing=$t"
  done
  anchor=$R
  while [ ! -e "$anchor" ] && [ "$anchor" != / ]; do anchor=$(dirname "$anchor"); done
  echo "root=$R"
  [ -d "$R" ] && echo "root_exists=1" || echo "root_exists=0"
  [ -d "$anchor" ] && [ -w "$anchor" ] && echo "root_writable=1" || echo "root_writable=0"
  echo "root_anchor=$anchor"
  df -Pk "$anchor" 2>/dev/null | awk 'NR == 2 { print "free_kb=" $4 }'
}

envgaps() {
  local n
  for n in $A1; do
    [ -n "${!n:-}" ] || echo "$n"
  done
  return 0
}

exec_script() {
  need_slot "$A1"; cd "$D" || fail "no such slot: $A1"
  export WORK="$D"
  bash -c "$A2"
}

case "$VERB" in
  list) list_slots ;;
  machine) need_slot "$A1"; if known; then emit "$A1"; fi ;;
  probe) probe ;;
  envgaps) envgaps ;;
  offer) offer ;;
  create) create_slot ;;
  start) start_slot ;;
  stop) stop_slot ;;
  claim) claim_slot ;;
  unclaim) need_slot "$A1"; rm -f "$D/claim" ;;
  put) put_file ;;
  seal) seal_mark ;;
  read) read_files ;;
  exec) exec_script ;;
  destroy) destroy_slot ;;
  *) fail "unknown verb: $VERB" ;;
esac
