#!/bin/bash
# provision-target.sh <full|review> <target> <remote> <root> [<warm-land>]
#
# Lay out one target on this host under <root> (the host's `root` in
# cluster.toml), as the worker expects it:
#
#   <root>/trees/<target>/repo   the pool's warm repository (a clone)
#   <root>/trees/<target>/check  full only: the check tree (a clone)
#   <root>/trees/<target>/land   full only: the gate tree (remote `lane`)
#   <root>/space/<target>        the local mailspace: pool `agent`, role `hand`
#   <root>/slots                 the pool's slot worktrees
#
# full:   an agent, check and gate host (the studios). <warm-land>, when
#         given, is an existing warm gate worktree that is moved into place
#         (a rename on the same drive: its build cache comes along). The
#         check tree's `target/` is then seeded from it with an APFS clone,
#         which shares blocks and writes almost nothing.
# review: a review-only host (the mini). The pool harness has no shell, so
#         it never builds; only the repository and the space are made.
#
# Safe to run again: each step checks before it changes anything.
set -euo pipefail
MODE=$1 TARGET=$2 REMOTE=$3 ROOT=$4 WARM=${5:-}
export PATH=$HOME/.local/bin:/opt/homebrew/bin:$PATH
T=$ROOT/trees/$TARGET
S=$ROOT/space/$TARGET
MODEL=opencode-go/deepseek-v4.1-flash
mkdir -p "$T" "$S" "$ROOT/slots"
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
  RUN="pi -p --no-session --model {model} @{brief}"
  ;;
review)
  RUN="pi -p --no-session --no-mcp --tools read,grep,find,ls --model {model} @{brief}"
  ;;
*)
  echo "mode must be full or review" >&2
  exit 1
  ;;
esac

step space
cd "$S"
[ -d .vivi ] || vivi mailspace init >/dev/null
if ! grep -q '^\[pools\.agent\]' .vivi/mailspace.toml; then
  vivi pool add --provider local --slots 2 --worktree-of "$T/repo" --root "$ROOT/slots" agent >/dev/null
fi
vivi pool set agent harness "{ name = \"pi\", run = \"$RUN\", models = [\"$MODEL\"] }" >/dev/null
vivi pool set agent committer '{ name = "Ian Zepp", email = "ian.zepp@protonmail.com" }' >/dev/null
grep -q '^name = "hand"' .vivi/mailspace.toml || vivi role add hand --kind hand >/dev/null

step done
sed -n '/^\[pools\.agent\]/,$p' .vivi/mailspace.toml
ls "$T"
