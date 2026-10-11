#!/bin/bash
# Vivi pool slot. Runs inside the machine on every boot.
#
# Environment (set by `vivi pool create`):
#   REPO        owner/name on GitHub
#   BASE        commit to seed at
#   GH_TOKEN    short-lived fetch token (first boot only)
#   BUILD_DIR   build output directory, relative to the checkout
#   TMPFS_SIZE  size of the memory disk mounted over BUILD_DIR
# Files written beside this script: /work/setup.sh (root, once) and
# /work/seed.sh (in the checkout, once).
#
# First boot seeds: tools, a shallow checkout at BASE, a warmed build output
# packed onto the persistent root disk. The machine then exits and stays
# stopped; a stopped machine is a free slot. Later boots restore the pack into
# a fresh memory disk, wait for one job, run it, and stop. A failed seed only
# costs time: the pack is still written, the exit code reports the failure.
L=/work/boot.log
RD=/work/repo
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
rm -rf /work/job; mkdir -p /work/job
cd "$RD" && mkdir -p "$BUILD_DIR" && mount -t tmpfs -o "size=$TMPFS_SIZE" tmpfs "$BUILD_DIR"
[ -f /work/build.tar.zst ] && zstd -d -T0 -c /work/build.tar.zst | tar -xf - -C "$RD"
chown -R hand:hand "$BUILD_DIR"
T1=$(now); echo "restore_s $(d $T0 $T1)" >> $L; echo "ready $T1" >> $L
# Wait for a job, bounded; an idle slot stops itself.
for i in $(seq 1 ${IDLE_LIMIT:-120}); do [ -f /work/job/go ] && break; sleep 1; done
[ -f /work/job/go ] || { echo "no_job_idle_stop" >> $L; exit 0; }
bash /work/job/job.sh > /work/job/job.log 2>&1; echo "job_rc $? $(now)" >> $L
sleep ${LINGER:-90}
