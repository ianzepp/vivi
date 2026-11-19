#!/bin/bash
# provision-target.sh <full|review> <target> <remote> <root> [<warm-land>]
#
# Lay out one target on this host under <root> (the host's `root` in
# cluster.toml), as the worker expects it:
#
#   <root>/trees/<target>/repo   the warm repository the agent worktrees
#                                come from (a clone)
#   <root>/trees/<target>/slots  the agent worktrees; the worker makes
#                                slot-<n> on first use
#   <root>/trees/<target>/check  full only: the check tree (a clone)
#   <root>/trees/<target>/land   full only: the gate tree (remote `lane`)
#
# full:   an agent, check and gate host (the studios). <warm-land>, when
#         given, is an existing warm gate worktree that is moved into place
#         (a rename on the same drive: its build cache comes along). The
#         check tree's `target/` is then seeded from it with an APFS clone,
#         which shares blocks and writes almost nothing.
# review: a review-only host (the mini). Its review harness has no shell,
#         so it never builds; only the repository and the slots directory
#         are made.
#
# Safe to run again: each step checks before it changes anything.
set -euo pipefail
MODE=$1 TARGET=$2 REMOTE=$3 ROOT=$4 WARM=${5:-}
export PATH=$HOME/.local/bin:/opt/homebrew/bin:$PATH
T=$ROOT/trees/$TARGET
mkdir -p "$T/slots"
step() { printf '== %s\n' "$*"; }

step repo
[ -d "$T/repo/.git" ] || git clone -q "$REMOTE" "$T/repo"

case "$MODE" in
full)
  step land
  if [ ! -e "$T/land" ]; then
    if [ -n "$WARM" ]; then
      common=$(git -C "$WARM" rev-parse --path-format=absolute --git-common-dir)
      git --git-dir="$common" worktree move "$WARM" "$T/land"
      git -C "$T/land" remote get-url lane >/dev/null
    else
      git clone -q -o lane "$REMOTE" "$T/land"
    fi
  fi
  step check
  if [ ! -d "$T/check/.git" ]; then
    git clone -q "$REMOTE" "$T/check"
    if [ -d "$T/land/target" ] && [ ! -e "$T/check/target" ]; then
      cp -cR "$T/land/target" "$T/check/target"
    fi
  fi
  ;;
review) ;;
*)
  echo "mode must be full or review" >&2
  exit 1
  ;;
esac

step done
ls "$T"
