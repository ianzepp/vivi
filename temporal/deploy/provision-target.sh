#!/bin/bash
# provision-target.sh <full|review> <remote> <root>
#
# Lay out one lane on this host under <root> (the host's `root` in
# cluster.toml), as the worker expects it. <owner>/<repo> comes from the
# remote, as in Config::for_host (S2), so every target on that remote
# shares these trees:
#
#   <root>/trees/<owner>/<repo>/repo   the warm repository the agent
#                                      worktrees come from (a clone)
#   <root>/trees/<owner>/<repo>/slots  the agent worktrees; the worker
#                                      makes slot-<n> on first use
#   <root>/trees/<owner>/<repo>/verify full only: the verification tree,
#                                      where check and gate both build
#                                      (a clone, remote `lane`)
#
# full:   an agent and verify host (the studios): the repository, the
#         slots directory and the verification tree. A moved tree must not
#         be reused as is: radix compiles its own path into its binaries.
# review: a review-only host (the mini). Its review harness has no shell,
#         so it never builds; only the repository and the slots directory
#         are made.
#
# Safe to run again: each step checks before it changes anything.
set -euo pipefail
MODE=$1 REMOTE=$2 ROOT=${3%/}
export PATH=$HOME/.local/bin:/opt/homebrew/bin:$PATH
# <owner>/<repo>: the last two segments after any ':', without .git.
lane=${REMOTE%/}; lane=${lane%.git}; lane=${lane##*:}
repo=${lane##*/}; owner=${lane%/*}; owner=${owner##*/}
case "$lane" in */*) ;; *) owner= ;; esac
case "$owner/$repo" in
''/* | */'' | ./* | ../* | */. | */..)
  echo "remote $REMOTE names no <owner>/<repo>" >&2
  exit 1
  ;;
esac
T=$ROOT/trees/$owner/$repo
mkdir -p "$T/slots"
step() { printf '== %s\n' "$*"; }

step repo
[ -d "$T/repo/.git" ] || git clone -q "$REMOTE" "$T/repo"

case "$MODE" in
full)
  step verify
  [ -d "$T/verify/.git" ] || [ -f "$T/verify/.git" ] || git clone -q -o lane "$REMOTE" "$T/verify"
  ;;
review) ;;
*)
  echo "mode must be full or review" >&2
  exit 1
  ;;
esac

step done
ls "$T"
