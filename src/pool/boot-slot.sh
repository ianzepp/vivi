#!/bin/bash
# Vivi pool slot. Runs inside the machine on every boot.
#
# Environment (set by `vivi pool create`):
#   REPO        owner/name on GitHub
#   BASE        commit to seed at
#   GH_TOKEN    short-lived fetch token (first boot only; `create` removes it
#               from the machine's config once the seed has finished)
#   BUILD_DIR   build output directory, relative to the checkout
#   TMPFS_SIZE  size of the memory disk mounted over BUILD_DIR
# Files written beside this script: /work/setup.sh (root, once) and
# /work/seed.sh (in the checkout, once).
#
# First boot seeds: tools, a shallow checkout at BASE, a warmed build output
# packed onto the persistent root disk. The machine then exits and stays
# stopped; a stopped machine is a free slot. A failed seed only costs time:
# the pack is still written, the exit code reports the failure.
#
# Later boots meet one of three situations, told apart by /work/job.tgz, the
# job archive that `vivi pool dispatch` writes into the machine's config:
#   new job      the archive differs from the last one run: unpack it, restore
#                the build output, run the job under a hard time limit, then
#                linger so the result can be collected without a restart.
#   old job      the archive is the one already run (a collect boot): do not
#                run it again; linger so the result can be read, then stop.
#   no job       idle: stop after IDLE_LIMIT seconds.
# The job directory is never wiped at boot, only replaced by the next job, so
# a result survives stopping and starting until it is collected.
L=/work/boot.log
RD=/work/repo
JT=/work/job.tgz
now() { date +%s.%N; }
d() { awk -v a="$1" -v b="$2" 'BEGIN{printf "%.1f", b-a}'; }
T0=$(now)

need() {
  command -v "$1" >/dev/null 2>&1 && return 0
  apt-get update -qq >/dev/null 2>&1
  apt-get install -y -qq --no-install-recommends "$2" >/dev/null 2>&1
  command -v "$1" >/dev/null 2>&1
}

if [ ! -f /work/seeded ]; then
  echo "seed_start $T0 cores $(nproc) base ${BASE:0:12}" >> $L
  for t in "zstd zstd" "git git" "curl curl"; do
    set -- $t
    need "$1" "$2" || { echo "seed_failed missing_tool $1" >> $L; exit 2; }
  done
  id hand >/dev/null 2>&1 || useradd -m hand
  bash /work/setup.sh > /work/setup.log 2>&1 || { echo "seed_failed setup rc=$?" >> $L; exit 3; }
  T1=$(now); echo "setup_s $(d $T0 $T1)" >> $L
  mkdir -p "$RD" && cd "$RD" && git init -q . \
    && git fetch -q --depth 1 "https://x-access-token:${GH_TOKEN}@github.com/${REPO}.git" "$BASE" \
    && git checkout -q FETCH_HEAD || { echo "seed_failed fetch" >> $L; exit 4; }
  rm -f "$RD/.git/FETCH_HEAD"
  git rev-parse HEAD > /work/seed-base.txt
  T2=$(now); echo "clone_s $(d $T1 $T2)" >> $L
  mkdir -p "$BUILD_DIR" && mount -t tmpfs -o "size=$TMPFS_SIZE" tmpfs "$BUILD_DIR"
  bash /work/seed.sh > /work/seed.log 2>&1; SRC=$?
  T3=$(now); echo "seed_rc $SRC seed_s $(d $T2 $T3) build_mb $(du -sm "$BUILD_DIR" | cut -f1)" >> $L
  tar -cf - "$BUILD_DIR" | zstd -T0 -3 -q -o /work/build.tar.zst.partial \
    && mv /work/build.tar.zst.partial /work/build.tar.zst
  chown -R hand:hand /home/hand "$RD"
  T4=$(now); echo "pack_s $(d $T3 $T4) pack_mb $(du -sm /work/build.tar.zst | cut -f1)" >> $L
  sync; touch /work/seeded; echo "seed_done $T4" >> $L
  exit "$SRC"
fi

echo "boot_start $T0" >> $L
LINGER=${LINGER:-120}

# The archive arrives with the machine's config on every boot; keep it out of
# reach of the agent by unpacking it and removing the copy at once.
MODE=idle
if [ -f "$JT" ]; then
  chmod 600 "$JT"
  SHA=$(sha256sum "$JT" | cut -d' ' -f1)
  if [ "$SHA" = "$(cat /work/job.sha 2>/dev/null)" ]; then
    MODE=old
  else
    MODE=new
  fi
fi

if [ "$MODE" = old ]; then
  rm -f "$JT"
  echo "collect_boot linger ${LINGER}s" >> $L
  sleep "$LINGER"; exit 0
fi

if [ "$MODE" = idle ]; then
  sleep "${IDLE_LIMIT:-120}"
  echo "no_job_idle_stop" >> $L; exit 0
fi

# New job. The sha is recorded before anything runs, so a crash mid-job is not
# mistaken for a job to run again.
rm -rf /work/job
tar -xzf "$JT" -C /work --no-same-owner && chmod 700 /work/job
rm -f "$JT"; echo "$SHA" > /work/job.sha
JOB=$(cat /work/job/id 2>/dev/null)
LIMIT=$(cat /work/job/limit 2>/dev/null)
case "$LIMIT" in ''|*[!0-9]*|0) LIMIT=3600 ;; esac
[ -f /work/job/run.sh ] || { echo "job_bad_archive $JOB" >> $L; exit 5; }

cd "$RD" && mkdir -p "$BUILD_DIR" && mount -t tmpfs -o "size=$TMPFS_SIZE" tmpfs "$BUILD_DIR"
[ -f /work/build.tar.zst ] && zstd -d -T0 -c /work/build.tar.zst | tar -xf - -C "$RD"
chown -R hand:hand "$BUILD_DIR"
T1=$(now); echo "restore_s $(d $T0 $T1)" >> $L; echo "ready $T1 job $JOB" >> $L

# Every job, whatever it is, runs under a hard time limit enforced here.
timeout -k 30 "$LIMIT" bash /work/job/run.sh > /work/job/run.log 2>&1; RC=$?
if [ ! -f /work/job/result ]; then
  printf 'job=%s\nstate=killed\npush=none\nrunner_rc=%s\n' "$JOB" "$RC" > /work/job/result
fi
echo "job_rc $RC job $JOB limit ${LIMIT}s $(now)" >> $L
sleep "$LINGER"
