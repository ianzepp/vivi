# Vivi worker driver for macOS. Not run on its own: the provider sends this
# text, after assignments of ROOT, VERB, UMASK and A1..A3, as the script of
# one `bash -c` on the worker (or of a plain `bash` on this machine), once per
# call. It is never installed, so it cannot be stale. Protocol:
# docs/worker-driver-protocol.md. Works with the stock /bin/bash 3.2.
#
# The worker offers capacity; the orchestrator asks for one bounded workspace
# per job and the driver builds it, runs the job inside, and tears it down.
# Boundary kinds:
#   ram   a RAM disk of size_gib (hdiutil + diskutil), freed by eject
#   apfs  an APFS volume with a quota of size_gib, created unmounted and then
#         mounted with noowners (no root needed). The volumes live in a sparse
#         disk image the driver creates and attaches itself (apfs_image),
#         because the system container refuses noowners mounts over SSH; the
#         image's size is the hard ceiling of all apfs boundaries together.
#         On a volume where the login user may create volumes and mount them
#         with noowners (an external drive, once SSH has full disk access),
#         apfs_volume names a mounted path and its own container is used.
#         An operator may also name a container directly (apfs_container).
#   dir   a plain directory, no limit, under dir_root (default $R/jobs)
#
# Layout under $R (ROOT, `~/` expanded):
#   worker.conf       operator budgets, key=value lines (see conf below)
#   .lock/            held by admit, release and sweep (mkdir is atomic)
#   ledger/<job>      one reservation per job: kind, size, device, mount, state
#   jobs/<job>/       the boundary's mount point (the job's workspace). macOS
#                     will not mount a volume inside another external volume,
#                     so $R belongs on the internal disk even when the
#                     volumes live on an external drive.
#   run/<job>/        pid, start, exit_code, boot.out of the job's boot
#
# Verbs:
#   offer                  capabilities and capacity
#   admit A1 A2            job A1, request A2 ("kind=apfs size_gib=2")
#   put A1 A2              write standard input to A2 inside job A1's boundary
#   start A1               run A1's boot.sh detached, inside the boundary
#   status A1              running, exited with its code, or gone
#   read A1 A2             tar stream of the files named in A2 that exist
#   release A1             stop everything on the boundary and tear it down
#   sweep A1               release every job not named in A1 (the live set)
umask "${UMASK:-077}"
die() { echo "vivi-worker: $*" >&2; exit 1; }

case "$ROOT" in
  "~/"*) R="$HOME/${ROOT#\~/}" ;;
  *) R=$ROOT ;;
esac
case "$R" in ''|/) die "the root is empty or /" ;; /*) ;; *) die "the root is not absolute: $R" ;; esac

job_ok() { [[ $1 =~ ^[a-z0-9][a-z0-9-]{0,62}$ ]]; }
need_job() { job_ok "$1" || die "not a job id: $1"; }

# One operator setting from worker.conf, else the default $2.
conf() {
  local v
  v=$(sed -n "s/^$1=//p" "$R/worker.conf" 2>/dev/null | tail -n1)
  printf '%s' "${v:-$2}"
}
# One field of a ledger entry.
field() { sed -n "s/^$2=//p" "$R/ledger/$1" 2>/dev/null | tail -n1; }

# Where dir boundaries live: dir_root, else under the root.
dir_root() {
  local d
  d=$(conf dir_root "$R/jobs")
  case "$d" in "~/"*) d="$HOME/${d#\~/}" ;; esac
  printf '%s' "$d"
}

# The worker lock. A lock older than ten minutes was left by a dead call.
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
  die "the worker lock is busy"
}

# GiB reserved by the ledger for boundary kind $1.
reserved() {
  local f sum=0 size
  for f in "$R"/ledger/*; do
    [ -f "$f" ] || continue
    [ "$(sed -n 's/^kind=//p' "$f")" = "$1" ] || continue
    size=$(sed -n 's/^size_gib=//p' "$f")
    sum=$((sum + ${size:-0}))
  done
  echo "$sum"
}
# Memory a new RAM disk could take now, in whole GiB: free, inactive and
# speculative pages.
mem_free_gib() {
  vm_stat | awk '
    /page size of/ { ps = $8 }
    /^Pages (free|inactive|speculative):/ { gsub(/\./, "", $NF); n += $NF }
    END { printf "%d\n", n * ps / 1073741824 }'
}
# The APFS container that holds apfs boundaries: the container of the
# mounted path apfs_volume (device names change across reboots, paths do
# not), or apfs_container as given, or the container of apfs_image, which is
# created at the apfs budget when it is missing and attached when it is not
# (attaching twice is harmless).
apfs_container() {
  local img part vol
  vol=$(conf apfs_volume '')
  if [ -n "$vol" ]; then
    diskutil info -plist "$vol" 2> /dev/null | plutil -extract APFSContainerReference raw -o - - 2> /dev/null
    return 0
  fi
  img=$(conf apfs_image '')
  if [ -z "$img" ]; then
    conf apfs_container ''
    return 0
  fi
  case "$img" in "~/"*) img="$HOME/${img#\~/}" ;; esac
  if [ ! -f "$img" ]; then
    hdiutil create -size "$(conf apfs_budget_gib 0)g" -type SPARSE -fs APFS \
      -volname vivi-workspace -quiet "$img" 2> /dev/null || return 1
  fi
  part=$(hdiutil attach -nomount "$img" 2> /dev/null | awk '$2 == "Apple_APFS" { print $1 }' | head -n1)
  [ -n "$part" ] || return 1
  diskutil info -plist "$part" | plutil -extract APFSContainerReference raw -o - - 2> /dev/null
}
# Free space of APFS container $1, in whole GiB.
apfs_free_gib() {
  diskutil apfs list -plist "$1" 2>/dev/null \
    | plutil -extract Containers.0.CapacityFree raw -o - - 2>/dev/null \
    | awk '{ printf "%d\n", $1 / 1073741824 }'
}

offer() {
  local container t tools=
  echo "system=$(uname -s) $(uname -m)"
  echo "kinds=ram apfs dir"
  echo "ram_budget_gib=$(conf ram_budget_gib 0) ram_reserved_gib=$(reserved ram) mem_free_gib=$(mem_free_gib)"
  container=$(apfs_container)
  if [ -n "$container" ]; then
    echo "apfs_container=$container apfs_budget_gib=$(conf apfs_budget_gib 0) apfs_reserved_gib=$(reserved apfs) apfs_free_gib=$(apfs_free_gib "$container")"
  fi
  command -v docker > /dev/null 2>&1 && docker info > /dev/null 2>&1 && echo "docker=1" || echo "docker=0"
  [ "$(uname -m)" = arm64 ] && echo "gpu=metal" || echo "gpu=none"
  for t in git cargo node python3 perl; do
    command -v "$t" > /dev/null 2>&1 && tools="$tools $t"
  done
  echo "tools=${tools# }"
}

# admit's answers.
undeliverable() { echo "result=undeliverable"; echo "reason=$*"; exit 0; }
admit_failed() { echo "result=failed"; echo "reason=$*"; exit 1; }

# Check request ($2 kind, $3 size) against budget and live capacity.
admit_check() {
  local kind=$1 size=$2 budget used free margin container
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
      container=$(apfs_container)
      [ -n "$container" ] || undeliverable "no apfs_image or apfs_container is configured"
      budget=$(conf apfs_budget_gib 0); used=$(reserved apfs)
      free=$(apfs_free_gib "$container")
      [ $((used + size)) -le "$budget" ] \
        || undeliverable "apfs ${size}g exceeds the budget (${used}g of ${budget}g reserved)"
      [ "$size" -le "${free:-0}" ] \
        || undeliverable "apfs ${size}g exceeds ${free:-0}g free in $container"
      ;;
    dir) ;;
    *) undeliverable "this worker offers no boundary of kind $kind" ;;
  esac
}

# Build the boundary of job $1 at mount point $2; prints "device=..." lines
# for the ledger. Undone by teardown when it fails part way.
build_ram() {
  local job=$1 at=$2 size=$3 disk volume
  disk=$(hdiutil attach -nomount "ram://$((size * 2097152))" | awk 'NR == 1 { print $1 }')
  [ -n "$disk" ] || return 1
  echo "device=$disk"
  diskutil eraseVolume APFS "vivi-$job" "$disk" > /dev/null || return 1
  volume=$(diskutil info -plist "/Volumes/vivi-$job" | plutil -extract DeviceIdentifier raw -o - - 2>/dev/null)
  [ -n "$volume" ] || return 1
  echo "volume=$volume"
  diskutil unmount "$volume" > /dev/null || return 1
  diskutil mount -mountPoint "$at" "$volume" > /dev/null || return 1
  : > "$at/.metadata_never_index"
}
build_apfs() {
  local job=$1 at=$2 size=$3 container volume
  container=$(apfs_container)
  [ -n "$container" ] || return 1
  volume=$(diskutil apfs addVolume "$container" APFS "vivi-$job" -quota "${size}g" -nomount \
    | sed -n 's/.*Created new APFS Volume \(disk[0-9]*s[0-9]*\).*/\1/p' | head -n1)
  [ -n "$volume" ] || return 1
  echo "volume=$volume"
  diskutil mount -mountOptions noowners -mountPoint "$at" "$volume" > /dev/null || return 1
  # A throwaway build volume is not worth indexing.
  : > "$at/.metadata_never_index"
}

admit() {
  local job=$A1 kind size at entry rc
  job_ok "$job" || admit_failed "not a job id: $job"
  kind=$(printf '%s\n' $A2 | sed -n 's/^kind=//p')
  size=$(printf '%s\n' $A2 | sed -n 's/^size_gib=//p')
  [[ ${size:-0} =~ ^[0-9]+$ ]] || admit_failed "size_gib is not a whole number: $size"
  size=${size:-0}
  lock
  [ ! -e "$R/ledger/$job" ] || admit_failed "job $job already has a boundary"
  admit_check "$kind" "$size"
  at="$R/jobs/$job"
  [ "$kind" != dir ] || at="$(dir_root)/$job"
  mkdir -p "$R/ledger" "$R/run/$job" "$at" || admit_failed "cannot make $at"
  # The reservation is written before anything is built, so a crash part way
  # leaves a ledger entry that release and sweep can clean up after.
  entry="$R/ledger/$job"
  printf 'kind=%s\nsize_gib=%s\nmount=%s\ncreated=%s\nstate=building\n' \
    "$kind" "$size" "$at" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$entry"
  case "$kind" in
    ram) build_ram "$job" "$at" "$size" >> "$entry"; rc=$? ;;
    apfs) build_apfs "$job" "$at" "$size" >> "$entry"; rc=$? ;;
    dir) rc=0 ;;
  esac
  if [ "$rc" -ne 0 ]; then
    teardown "$job"
    admit_failed "could not build the $kind boundary"
  fi
  echo "state=ready" >> "$entry"
  echo "result=admitted"
  echo "path=$at"
}

# The pids with any file, or their working directory, inside job $1.
users_of() {
  local at kind
  at=$(field "$1" mount); kind=$(field "$1" kind)
  [ -n "$at" ] && [ -d "$at" ] || return 0
  if [ "$kind" = dir ]; then
    lsof -t +D "$at" 2>/dev/null
  else
    lsof -t +f -- "$at" 2>/dev/null
  fi | sort -un
}

# Stop everything on job $1's boundary, then remove the boundary and its
# ledger entry. Only devices the ledger recorded are touched, and an APFS
# volume only while it still carries the job's vivi- name.
teardown() {
  local job=$1 kind at device volume sig pids _ name
  [ -f "$R/ledger/$job" ] || return 0
  kind=$(field "$job" kind); at=$(field "$job" mount)
  device=$(field "$job" device); volume=$(field "$job" volume)
  for sig in TERM KILL; do
    pids=$(users_of "$job" | tr '\n' ' ')
    [ -n "${pids// /}" ] || break
    kill -"$sig" $pids 2>/dev/null
    for _ in $(seq 50); do
      [ -n "$(users_of "$job")" ] || break
      sleep 0.1
    done
  done
  case "$kind" in
    ram)
      [ -z "$device" ] || diskutil eject "$device" > /dev/null 2>&1
      ;;
    apfs)
      if [ -n "$volume" ]; then
        name=$(diskutil info -plist "$volume" 2>/dev/null | plutil -extract VolumeName raw -o - - 2>/dev/null)
        if [ "$name" = "vivi-$job" ]; then
          diskutil unmount force "$volume" > /dev/null 2>&1
          diskutil apfs deleteVolume "$volume" > /dev/null 2>&1 || return 1
        fi
      fi
      ;;
  esac
  case "$at" in "$R"/jobs/*|"$(dir_root)"/*) rm -rf -- "$at" ;; esac
  rm -rf -- "$R/run/$job"
  rm -f -- "$R/ledger/$job"
}

release() {
  need_job "$A1"
  lock
  [ -f "$R/ledger/$A1" ] || { echo "result=gone"; return 0; }
  teardown "$A1" || die "could not tear down $A1"
  echo "result=released"
}

sweep() {
  local f job keep
  lock
  for f in "$R"/ledger/*; do
    [ -f "$f" ] || continue
    job=${f##*/}
    keep=0
    for live in $A1; do [ "$live" = "$job" ] && keep=1; done
    [ "$keep" = 1 ] && continue
    teardown "$job" && echo "released $job"
  done
  return 0
}

# The boot wrapper of job $1 is running: its pid is alive and still has the
# start time recorded when it began (a reused pid has another).
# A boot that ended before its start time was read leaves the start empty; an
# empty time never matches, and a written exit code always means it ended.
running() {
  local p now
  [ ! -f "$R/run/$1/exit_code" ] || return 1
  p=$(cat "$R/run/$1/pid" 2>/dev/null) || return 1
  [[ $p =~ ^[0-9]+$ ]] || return 1
  now=$(ps -o lstart= -p "$p" 2>/dev/null)
  [ -n "$now" ] && [ "$now" = "$(cat "$R/run/$1/start" 2>/dev/null)" ]
}

start() {
  local at run _
  need_job "$A1"
  [ "$(field "$A1" state)" = ready ] || die "job $A1 has no ready boundary"
  running "$A1" && return 0
  at=$(field "$A1" mount); run="$R/run/$A1"
  [ -f "$at/boot.sh" ] || die "job $A1 has no boot.sh"
  rm -f "$run/pid" "$run/start" "$run/exit_code"
  (
    cd "$at" || exit 1
    export WORK="$at" VIVI_JOB="$A1" VIVI_RUN="$run"
    nohup perl -MPOSIX -e 'POSIX::setsid() or die "setsid: $!\n"; exec @ARGV or die "exec: $!\n"' \
      bash -l -c 'echo $$ > "$VIVI_RUN/pid"; bash "$WORK/boot.sh"; rc=$?; echo "$rc" > "$VIVI_RUN/exit_code"; exit "$rc"' \
      > "$run/boot.out" 2>&1 < /dev/null &
  )
  for _ in $(seq 100); do
    if [ -s "$run/pid" ]; then
      ps -o lstart= -p "$(cat "$run/pid")" > "$run/start" 2>/dev/null
      return 0
    fi
    sleep 0.05
  done
  die "job $A1 did not start (see $run/boot.out)"
}

status() {
  need_job "$A1"
  if [ ! -f "$R/ledger/$A1" ]; then echo "state=gone"
  elif running "$A1"; then echo "state=running"
  elif [ -f "$R/run/$A1/exit_code" ]; then echo "state=exited"; echo "exit=$(head -n1 "$R/run/$A1/exit_code")"
  else echo "state=$(field "$A1" state)"
  fi
}

put() {
  local at
  need_job "$A1"
  at=$(field "$A1" mount)
  [ -n "$at" ] && [ -d "$at" ] || die "job $A1 has no boundary"
  case "$A2" in ''|/*|*..*) die "bad path: $A2" ;; esac
  mkdir -p "$(dirname "$at/$A2")" && cat > "$at/$A2.partial" && mv -f "$at/$A2.partial" "$at/$A2" \
    || die "cannot write $A2"
}

read_files() {
  local at p list=
  need_job "$A1"
  at=$(field "$A1" mount)
  cd "$at" 2>/dev/null || return 0
  for p in $A2; do
    case "$p" in ''|/*|*..*) continue ;; esac
    [ -f "$p" ] && list="$list $p"
  done
  [ -z "$list" ] || tar -cf - $list
}

case "$VERB" in
  offer) offer ;;
  admit) admit ;;
  put) put ;;
  start) start ;;
  status) status ;;
  read) read_files ;;
  release) release ;;
  sweep) sweep ;;
  *) die "unknown verb: $VERB" ;;
esac
