#!/bin/bash
# Vivi pool slot. Runs inside the machine on every boot.
#
# Environment (set by `vivi pool create`):
#   REMOTE      full Git address of the source: an https address, or a file://
#               address of a bare repository on this disk
#   BASE        commit or branch to seed at
#   GH_TOKEN    short-lived fetch token, used only with an https REMOTE (first
#               boot only; `create` removes it from the machine's config once
#               the seed has finished). Absent for a file:// REMOTE.
#   SOURCE_DIR  optional: a source tree delivered with the slot. The seed turns
#               it into the bare repository at REMOTE (branch main) first.
#   BUILD_DIR   build output directory, relative to the checkout. Unset when
#               the pool has none: then there is no memory disk, no pack, no
#               restore and nothing to keep out of the clean.
#   TMPFS_SIZE  size of the memory disk mounted over BUILD_DIR
#   WORKTREE_OF set when the provider already made the checkout $WORK/repo as a
#               Git worktree of a repository on this machine, at BASE: the
#               seed then fetches nothing and only warms the build.
#   BUILD_MOUNTED  set when the machine host already mounts that memory disk
#               (a container cannot mount without privileges): the script then
#               leaves the mount alone and only makes sure the directory exists.
#   BUILD_PERSISTS set when the build directory is a plain directory that
#               keeps its contents between boots: no memory disk, nothing is
#               packed or restored, a seeded slot simply keeps its warm build.
#   UNPRIVILEGED   set when the script runs as an ordinary user on a host it
#               does not own: no user is created, nothing is installed or
#               mounted, no ownership changes; a missing required tool is a
#               seed failure.
#   WORK        the slot's root directory (default /work). Every path below
#               lives under it. A `/work` prefix in REMOTE (file://) and in
#               SOURCE_DIR means this root, wherever it really is.
# A host whose machines cannot change their environment after creation delivers
# GH_TOKEN as $WORK/once.env instead; it is read and removed on first sight.
# Files written beside this script: $WORK/setup.sh (root, once),
# $WORK/requires.txt (optional: commands that must pass, see check_requires)
# and $WORK/seed.sh (in the checkout, once). A host that lacks GNU tools (timeout,
# sha256sum, nproc) puts stand-ins in $WORK/bin; they come last on PATH, so a
# real tool always wins.
#
# First boot seeds: tools, a shallow checkout at BASE, a warmed build output
# packed onto the persistent root disk. The machine then exits and stays
# stopped; a stopped machine is a free slot. A failed seed only costs time:
# the pack is still written, the exit code reports the failure.
#
# Later boots meet one of three situations, told apart by $WORK/job.tgz, the
# job archive that `vivi pool dispatch` writes into the machine's config:
#   new job      the archive differs from the last one run: unpack it, restore
#                the build output, run the job under a hard time limit, then
#                linger so the result can be collected without a restart.
#   old job      the archive is the one already run (a collect boot): do not
#                run it again; stay up IDLE_LIMIT seconds so the result can be
#                read, then stop.
#   no job       idle: stop after IDLE_LIMIT seconds.
# A boot with nothing to run never stays up longer than IDLE_LIMIT (default 60):
# whoever started it reads and stops it at once, and this is the backstop for
# a collector that died first.
# The job directory is never wiped at boot, only replaced by the next job, so
# a result survives stopping and starting until it is collected.
W=${WORK:-/work}
[ ! -d "$W/bin" ] || PATH=$PATH:$W/bin
L=$W/boot.log
RD=$W/repo
JT=$W/job.tgz
# A path as the configuration spells it (under /work) -> where it really is.
real() { case "$1" in /work|/work/*) echo "$W${1#/work}" ;; *) echo "$1" ;; esac; }
# `date +%N` is GNU only; elsewhere the fraction is left out.
now() { local t; t=$(date +%s.%N); echo "${t%.N}"; }
d() { awk -v a="$1" -v b="$2" 'BEGIN{printf "%.1f", b-a}'; }
T0=$(now)
if [ -f "$W/once.env" ]; then . "$W/once.env"; rm -f "$W/once.env"; fi

# The memory disk over the build output: mounted here unless the host did it.
mount_build() {
  [ -n "${BUILD_DIR:-}" ] || return 0
  mkdir -p "$BUILD_DIR"
  [ -n "${BUILD_MOUNTED:-}${BUILD_PERSISTS:-}" ] || mount -t tmpfs -o "size=$TMPFS_SIZE" tmpfs "$BUILD_DIR"
}

# The address to fetch from: an https remote gets the token, anything else is
# used as given.
remote_url() {
  case "$REMOTE" in
    https://*) [ -n "${GH_TOKEN:-}" ] && echo "https://x-access-token:${GH_TOKEN}@${REMOTE#https://}" || echo "$REMOTE" ;;
    *) echo "$REMOTE" ;;
  esac
}
LOCAL_REMOTE=
case "$REMOTE" in
  file://*) LOCAL_REMOTE=$(real "${REMOTE#file://}"); REMOTE=file://$LOCAL_REMOTE ;;
esac

# Requirements ($WORK/requires.txt, one command per line): each must exit 0 as
# the user the worker runs as (`hand`, or the current user when UNPRIVILEGED),
# in the checkout, under a time limit. The first failure ends the seed with
# exit 7 and a one-line reason in $WORK/seed.failure; when all pass, the list
# that passed is left in $WORK/requires.ok.
check_requires() {
  [ -s "$W/requires.txt" ] || return 0
  local as=() cmd rc last
  [ -n "${UNPRIVILEGED:-}" ] || as=(runuser -u hand --)
  while IFS= read -r cmd || [ -n "$cmd" ]; do
    [ -n "$cmd" ] || continue
    ( ulimit -f 8192; cd "$RD" && timeout -k 5 120 "${as[@]}" bash -c "$cmd" ) \
      > "$W/requires.out" 2>&1 < /dev/null
    rc=$?
    [ "$rc" -ne 0 ] || continue
    last=$(grep . "$W/requires.out" 2>/dev/null | tail -n 1 | tr -d '\000-\010\013-\037' | cut -c1-200)
    echo "requires: $(printf '%s' "$cmd" | cut -c1-200) exited $rc${last:+: $last}" > "$W/seed.failure"
    echo "seed_failed $(cat "$W/seed.failure")" >> $L
    return 1
  done < "$W/requires.txt"
  cp "$W/requires.txt" "$W/requires.ok"
}

need() {
  command -v "$1" >/dev/null 2>&1 && return 0
  [ -z "${UNPRIVILEGED:-}" ] || return 1
  apt-get update -qq >/dev/null 2>&1
  # A bare base image has no CA bundle; without one every https fetch fails.
  apt-get install -y -qq --no-install-recommends "$2" ca-certificates >/dev/null 2>&1
  command -v "$1" >/dev/null 2>&1
}

if [ ! -f "$W/seeded" ]; then
  rm -f "$W/seed.failure" "$W/requires.ok"
  echo "seed_start $T0 cores $(nproc) base ${BASE:0:12}" >> $L
  # tar and gzip unpack every job archive; a host without them fails here, clearly.
  TOOLS="git tar gzip"
  [ -n "${BUILD_PERSISTS:-}" ] || [ -z "${BUILD_DIR:-}" ] || TOOLS="$TOOLS zstd"
  [ -n "${UNPRIVILEGED:-}" ] || TOOLS="$TOOLS curl"
  for t in $TOOLS; do
    need "$t" "$t" || { echo "seed_failed missing_tool $t" >> $L; exit 2; }
  done
  [ -n "${UNPRIVILEGED:-}" ] || id hand >/dev/null 2>&1 || useradd -m hand
  bash "$W/setup.sh" > "$W/setup.log" 2>&1 || { echo "seed_failed setup rc=$?" >> $L; exit 3; }
  T1=$(now); echo "setup_s $(d $T0 $T1)" >> $L
  if [ -z "${REMOTE:-}${WORKTREE_OF:-}${SOURCE_DIR:-}" ]; then
    # Shared capacity is usable before any repository target is selected.
    mkdir -p "$RD"
    [ -n "${UNPRIVILEGED:-}" ] || chown -R hand:hand /home/hand "$RD"
    sync; touch "$W/seeded"; echo "seed_done $(now)" >> "$L"
    exit 0
  fi
  if [ -n "${SOURCE_DIR:-}" ] && [ -n "$LOCAL_REMOTE" ]; then
    SRC_DIR=$(real "$SOURCE_DIR")
    ( git init -q --bare "$LOCAL_REMOTE" \
      && git -C "$LOCAL_REMOTE" symbolic-ref HEAD refs/heads/main \
      && cd "$SRC_DIR" && git init -q . && git symbolic-ref HEAD refs/heads/main \
      && git add -A \
      && git -c user.name=vivi-pool -c user.email=pool@localhost commit -q -m "Embedded source" \
      && git push -q "$LOCAL_REMOTE" HEAD:refs/heads/main ) || { echo "seed_failed source" >> $L; exit 6; }
  fi
  if [ -n "${WORKTREE_OF:-}" ]; then
    # The provider made the checkout as a worktree of a local repository.
    [ -e "$RD/.git" ] || { echo "seed_failed worktree_missing" >> $L; exit 4; }
    cd "$RD"
  else
    mkdir -p "$RD" && cd "$RD" && git init -q . \
      && git fetch -q --depth "${DEPTH:-50}" "$(remote_url)" "$BASE" 2> "$W/fetch.err" \
      && git checkout -q FETCH_HEAD || {
        # Say why, without the address: it can carry the fetch token.
        echo "seed_failed fetch: $(grep -v '://' "$W/fetch.err" 2>/dev/null | tail -1 | cut -c1-160)" >> $L; exit 4; }
  fi
  rm -f "$RD/.git/FETCH_HEAD"
  git rev-parse HEAD > "$W/seed-base.txt"
  T2=$(now); echo "clone_s $(d $T1 $T2)" >> $L
  mount_build
  check_requires || exit 7
  bash "$W/seed.sh" > "$W/seed.log" 2>&1; SRC=$?
  T3=$(now); echo "seed_rc $SRC seed_s $(d $T2 $T3) build_mb $(du -sm "$BUILD_DIR" | cut -f1)" >> $L
  if [ -z "${BUILD_PERSISTS:-}" ]; then
    tar -cf - "$BUILD_DIR" | zstd -T0 -3 -q -o "$W/build.tar.zst.partial" \
      && mv "$W/build.tar.zst.partial" "$W/build.tar.zst"
    T4=$(now); echo "pack_s $(d $T3 $T4) pack_mb $(du -sm "$W/build.tar.zst" | cut -f1)" >> $L
  fi
  [ -n "${UNPRIVILEGED:-}" ] || chown -R hand:hand /home/hand "$RD" ${LOCAL_REMOTE:+"$LOCAL_REMOTE"}
  T4=$(now)
  sync; touch "$W/seeded"; echo "seed_done $T4" >> $L
  exit "$SRC"
fi

echo "boot_start $T0" >> $L
LINGER=${LINGER:-120}
IDLE_LIMIT=${IDLE_LIMIT:-60}

# The archive arrives with the machine's config on every boot; keep it out of
# reach of the agent by unpacking it and removing the copy at once.
MODE=idle
if [ -f "$JT" ]; then
  chmod 600 "$JT"
  SHA=$(sha256sum "$JT" | cut -d' ' -f1)
  if [ "$SHA" = "$(cat "$W/job.sha" 2>/dev/null)" ]; then
    MODE=old
  else
    MODE=new
  fi
fi

if [ "$MODE" = old ]; then
  rm -f "$JT"
  echo "collect_boot stay ${IDLE_LIMIT}s" >> $L
  sleep "$IDLE_LIMIT"; exit 0
fi

if [ "$MODE" = idle ]; then
  sleep "$IDLE_LIMIT"
  echo "no_job_idle_stop" >> $L; exit 0
fi

# New job. The sha is recorded before anything runs, so a crash mid-job is not
# mistaken for a job to run again.
rm -rf "$W/job"
tar -xzf "$JT" -C "$W" --no-same-owner && chmod 700 "$W/job"
rm -f "$JT"; echo "$SHA" > "$W/job.sha"
JOB=$(cat "$W/job/id" 2>/dev/null)
LIMIT=$(cat "$W/job/limit" 2>/dev/null)
case "$LIMIT" in ''|*[!0-9]*|0) LIMIT=3600 ;; esac
[ -f "$W/job/run.sh" ] || { echo "job_bad_archive $JOB" >> $L; exit 5; }

if [ ! -f "$W/job/checkout-key" ]; then
cd "$RD" && mount_build
if [ -z "${BUILD_PERSISTS:-}" ] && [ -n "${BUILD_DIR:-}" ]; then
  [ -f "$W/build.tar.zst" ] && zstd -d -T0 -c "$W/build.tar.zst" | tar -xf - -C "$RD"
  chown -R hand:hand "$BUILD_DIR"
fi
fi
T1=$(now); echo "restore_s $(d $T0 $T1)" >> $L; echo "ready $T1 job $JOB" >> $L

# Every job, whatever it is, runs under a hard time limit enforced here.
timeout -k 30 "$LIMIT" bash "$W/job/run.sh" > "$W/job/run.log" 2>&1; RC=$?
if [ ! -f "$W/job/result" ]; then
  printf 'job=%s\nstate=killed\npush=none\nrunner_rc=%s\n' "$JOB" "$RC" > "$W/job/result"
  if [ -f "$W/job/target-id" ]; then
    printf 'target=%s\nrepository=%s\n' "$(cat "$W/job/target-id")" "$(cat "$W/job/repository")" >> "$W/job/result"
  fi
fi
echo "job_rc $RC job $JOB limit ${LIMIT}s $(now)" >> $L
sleep "$LINGER"
