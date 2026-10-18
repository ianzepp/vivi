#!/bin/bash
# Vivi pool job runner. Runs inside a slot as root (or, where UNPRIVILEGED is
# set, as the ordinary user that owns the slot), started by the boot script
# under a hard time limit. Everything it needs is in $WORK/job (default
# /work/job), delivered as one archive:
#   params.env   REMOTE FROM BRANCH DEPTH BUDGET MODEL SLOT SUBJECT JOB TASK
#                COMMIT_NAME COMMIT_EMAIL ENV_NAMES
#                READ_ONLY (set to 1 for a read-only job; BRANCH is then empty)
#                (REMOTE is the full Git address; https gets the token below,
#                a file:// address is used as given). A worktree slot has no
#                REMOTE and also gets LOCAL_ACTION (create | continue) and
#                LOCAL_START (the commit FROM resolved to). With a bridge
#                grant also BRIDGE_SERVER and BRIDGE_PROJECT (not secret).
#   token        Git token, empty for a file:// remote; root-only, never
#                copied to the agent
#   bridge.key   the job's project API key for `vivi mcp`, present only when
#                the pool mints one; root-only, never copied to the agent,
#                given to the harness only as VIVI_MCP_API_KEY (with
#                VIVI_MCP_SERVER and VIVI_MCP_PROJECT_ID) and never to the
#                facts
#   mcp.json     present only with a bridge grant: the MCP configuration,
#                which names the key only by reference (${VIVI_MCP_API_KEY}),
#                copied into the agent's home next to brief.md and rules.md
#                (a harness template names it as {mcp_config})
#   harness.cmd  the rendered harness command
#   brief.md     the task body (with a grant: a pointer naming the task
#                handle; the text is on the server)
#   rules.md     the standing rules
#   facts.cmd    optional: commands to record before the harness starts, one
#                per line (see run_facts below)
#   restore.list optional: repository-relative paths to put back to the start
#                commit before the runner commits, one per line (written jobs
#                only; see the restore loop below)
#   with.list    optional: extra refs the agent must see, one `<ref> <commit>`
#                per line -- a branch name, or a 40-hex commit, then the
#                commit dispatch resolved it to (or `-`); fetch mode deepens
#                until each has a merge base with FROM (see the with loop
#                below)
# Environment from the machine: BUILD_DIR, and any variable named in
# ENV_NAMES (their values come from the machine's own environment).
# UNPRIVILEGED, BUILD_PERSISTS and WORK are the ones of the boot script.
#
# Start from FROM (a commit or a branch), run the harness as the unprivileged
# user `hand` (the current user when UNPRIVILEGED) under BUDGET seconds, then
# commit what changed and push to BRANCH without force. A commit the agent
# made on its own -- a merge of an extra ref, say -- is pushed the same way.
# The outcome goes to $WORK/job/result as key=value lines.
#
# A worktree slot (LOCAL_ACTION set) has no remote: the checkout is a Git
# worktree of a repository on this machine, so nothing is fetched or pushed.
# The work happens on a detached HEAD at LOCAL_START. Where LOCAL_ACTION is
# `create`, BRANCH is made only when there is something to commit, at the
# commit the work started from (a plain `checkout -b`, which refuses an
# existing name); where it is `continue`, BRANCH is checked out as it is
# (refused when another worktree holds it). The result is a commit on BRANCH
# (`push=committed`), and the worktree goes back to a detached HEAD at the
# end, whatever the outcome, so the branch is free again.
#
# A read-only job (READ_ONLY set) publishes nothing. FROM is checked out
# detached and no branch is made or moved. After the agent, the runner never
# commits and never pushes: it lists the paths that differ from FROM (tracked
# changes, commits the agent made, untracked files; not the build directory),
# restores the tree, and ends with push=read_only_unchanged or
# push=read_only_discarded plus discarded_count and the first
# $READ_ONLY_LIST_CAP paths as discarded_path_N.
#
# Where UNPRIVILEGED is set the agent is the same user as this script and
# there is no `hand` home: the brief and the rules go to $WORK/home (the
# harness command's /home/hand/ paths are rewritten to it), the agent keeps the
# user's own HOME and PATH, the Git token file is removed as soon as it is read,
# and whatever the agent left running is found by the VIVI_AGENT_JOB variable
# that only its environment carries. The bridge.key file, when there is one,
# is removed as soon as it is read, like the token.
W=${WORK:-/work}
J=$W/job
RD=$W/repo
. "$J/params.env"
# A file:// address under /work means this slot's root, wherever it really is.
case "$REMOTE" in
  file:///work|file:///work/*) REMOTE=file://$W${REMOTE#file:///work} ;;
esac
now() { date +%s; }
T0=$(now)
START=
AGENT_RC=
AGENT_S=
if [ -n "${UNPRIVILEGED:-}" ]; then
  as_hand() { "$@"; }
  AGENT_HOME=$W/home
  AGENT_USER_HOME=${HOME:-$AGENT_HOME}
  kill_agent() {
    local pids
    pids=$(grep -lzxF "VIVI_AGENT_JOB=$JOB" /proc/[0-9]*/environ 2>/dev/null | sed 's#/proc/\([0-9]*\)/environ#\1#')
    [ -z "$pids" ] || kill -KILL $pids 2>/dev/null
    return 0
  }
else
  as_hand() { runuser -u hand -- "$@"; }
  AGENT_HOME=/home/hand
  AGENT_USER_HOME=/home/hand
  kill_agent() { pkill -KILL -u hand 2>/dev/null; }
fi

# A worktree slot at rest is detached, so the branch it worked on is free to
# be checked out or merged elsewhere. Never touches the working tree.
detach_local() {
  [ -z "${LOCAL_ACTION:-}" ] || as_hand git -C "$RD" checkout -q --detach 2>/dev/null
  return 0
}
[ -z "${LOCAL_ACTION:-}" ] || trap detach_local EXIT
[ -z "${LOCAL_ACTION:-}" ] || trap 'exit 143' TERM HUP INT

finish() { # finish <push-outcome> [sha]
  detach_local
  {
    echo "job=$JOB"
    [ -z "${TARGET:-}" ] || echo "target=$TARGET"
    [ -z "${REPOSITORY:-}" ] || echo "repository=$REPOSITORY"
    echo "state=done"
    echo "push=$1"
    echo "branch=$BRANCH"
    echo "sha=${2:-}"
    echo "start=$START"
    echo "agent_rc=$AGENT_RC"
    echo "agent_s=$AGENT_S"
    echo "job_s=$(( $(now) - T0 ))"
    echo "model=$MODEL"
    echo "slot=$SLOT"
    [ -z "${RESULT_EXTRA:-}" ] || printf '%s\n' "$RESULT_EXTRA"
    [ -z "${RESTORE_EXTRA:-}" ] || printf '%s\n' "$RESTORE_EXTRA"
    [ -z "${FACTS_EXTRA:-}" ] || printf '%s\n' "$FACTS_EXTRA"
  } > "$J/result.partial"
  mv "$J/result.partial" "$J/result"
  exit 0
}

# A shared slot keeps repository caches isolated; never recycle another
# target's Git metadata or ignored build output.
if [ -n "${CHECKOUT_KEY:-}" ]; then
  [[ "$CHECKOUT_KEY" =~ ^[0-9a-f]{64}$ ]] || finish target_key_invalid
  CACHE=$W/targets/$CHECKOUT_KEY
  RD=$CACHE/repo
  if [ -L "$W/targets" ] || [ -L "$CACHE" ] || [ -L "$RD" ]; then
    finish target_cache_invalid
  fi
  if [ -n "${TARGET_LOCAL_SOURCE:-}" ]; then
    [ -f "$CACHE/worktree_of" ] && [ "$(cat "$CACHE/worktree_of")" = "$TARGET_LOCAL_SOURCE" ] || finish target_source_mismatch
    [ -e "$RD/.git" ] || finish target_worktree_missing
  else
    mkdir -p "$RD" || finish target_prepare_failed
    [ -n "${UNPRIVILEGED:-}" ] || chown -R hand:hand "$CACHE"
    if [ ! -d "$RD/.git" ]; then
      as_hand git -C "$RD" init -q . || finish target_prepare_failed
      printf '%s' "$REPOSITORY" > "$CACHE/source"
    fi
    [ "$(cat "$CACHE/source" 2>/dev/null)" = "$REPOSITORY" ] || finish target_source_mismatch
  fi
  # Target caches are persistent directories, independent of the legacy
  # pool build pack or mount under /work/repo.
  BUILD_PERSISTS=1
fi

cd "$RD" || finish workdir_missing
TOKEN=$(cat "$J/token")
[ -z "${UNPRIVILEGED:-}" ] || rm -f "$J/token"
BRIDGE_KEY=
if [ -f "$J/bridge.key" ]; then
  BRIDGE_KEY=$(cat "$J/bridge.key")
  [ -z "${UNPRIVILEGED:-}" ] || rm -f "$J/bridge.key"
fi
URL=$REMOTE
case "$REMOTE" in
  https://*) [ -n "$TOKEN" ] && URL="https://x-access-token:${TOKEN}@${REMOTE#https://}" ;;
esac

# A reused slot holds the last task's branch and stray files: return to a
# clean tree at FROM before anything runs.
CLEAN=(-fdq)
[ -z "${BUILD_PERSISTS:-}" ] || [ -z "${BUILD_DIR:-}" ] || CLEAN+=(-e "/$BUILD_DIR")
if [ -n "${LOCAL_ACTION:-}" ]; then
  START=$LOCAL_START
  as_hand git checkout -q -f --detach "$START" && as_hand git clean "${CLEAN[@]}" || finish checkout_failed
  if [ "$LOCAL_ACTION" = continue ]; then
    as_hand git checkout -q "$BRANCH" || finish checkout_failed
    START=$(as_hand git rev-parse HEAD)
  fi
else
  as_hand git fetch -q --depth "${DEPTH:-50}" "$URL" "$FROM" || finish fetch_failed
  START=$(as_hand git rev-parse FETCH_HEAD)
  as_hand git checkout -q -f "$START" && as_hand git clean "${CLEAN[@]}" || finish checkout_failed
  [ -n "${READ_ONLY:-}" ] || as_hand git checkout -q -B "$BRANCH" || finish checkout_failed
  rm -f "$RD/.git/FETCH_HEAD"
fi

# Run shared repository preparation once per target cache, only after the
# selected source and commit have been established.
if [ -n "${CHECKOUT_KEY:-}" ] && [ ! -f "$CACHE/prepared" ]; then
  if [ -f "$J/target-requires.txt" ]; then
    while IFS= read -r requirement || [ -n "$requirement" ]; do
      [ -z "$requirement" ] || as_hand bash -c "$requirement" || finish target_requirement_failed
    done < "$J/target-requires.txt"
  fi
  [ ! -f "$J/target-seed.sh" ] || as_hand bash "$J/target-seed.sh" || finish target_seed_failed
  as_hand git reset -q --hard "$START" && as_hand git clean "${CLEAN[@]}" || finish checkout_failed
  touch "$CACHE/prepared"
fi

# Extra refs the pool names in with.list: a second branch the agent must be
# able to see (and later merge). Fetch mode fetches each into a local ref --
# a branch name to refs/remotes/origin/<ref>, a 40-hex commit to
# refs/vivi/with/<commit> -- at the pool's depth. Every fetch, like the FROM
# fetch, leaves the URL, which holds the token, in .git/FETCH_HEAD, so
# FETCH_HEAD is removed after each one, and nothing is written to .git/config
# or the agent's home. The result records where each ref landed. A worktree
# job fetches nothing: the refs are already refs of the shared repository,
# and the agent sees them as named. A ref the remote does not have ends the
# job fetch_failed.
WITH_N=0
WITH_EXTRA=
WITH_PAIRS=
if [ -f "$J/with.list" ]; then
  while IFS=' ' read -r wref wsha || [ -n "$wref" ]; do
    [ -n "$wref" ] || continue
    WITH_N=$((WITH_N + 1))
    if [ -n "${LOCAL_ACTION:-}" ]; then
      wlocal=$wref
      wsha_now=$(as_hand git rev-parse --verify "$wref^{commit}" 2>/dev/null) || wsha_now=
    else
      if [ "${#wref}" -eq 40 ] && [ -z "${wref//[0-9a-f]/}" ]; then
        wlocal="refs/vivi/with/$wref"
      else
        wlocal="refs/remotes/origin/$wref"
      fi
      as_hand git fetch -q --depth "${DEPTH:-50}" "$URL" "+$wref:$wlocal" || finish fetch_failed
      rm -f "$RD/.git/FETCH_HEAD"
      wsha_now=$(as_hand git rev-parse "$wlocal")
    fi
    [ -z "$WITH_PAIRS" ] || WITH_PAIRS+=$'\n'
    WITH_PAIRS+="$wref $wlocal"
    [ -z "$WITH_EXTRA" ] || WITH_EXTRA+=$'\n'
    WITH_EXTRA+="with_${WITH_N}_ref=$wref"$'\n'"with_${WITH_N}_local=$wlocal"$'\n'"with_${WITH_N}_sha=$wsha_now"
  done < "$J/with.list"
  # The fetches are shallow: a merge base further back than the pool's depth
  # would make `git merge` of an extra ref refuse unrelated histories. While
  # any ref has no merge base with START, double the depth and fetch again --
  # FROM by its commit and every extra ref -- up to a total depth of 5000;
  # never --unshallow, never the whole history. A ref that still has no merge
  # base at the cap is only recorded: merging it is the agent's move. The
  # checkout persists between jobs, so a deeper fetch is incremental. The
  # result says to what depth the history reached and where each base sits.
  if [ -z "${LOCAL_ACTION:-}" ] && [ -n "$WITH_N" ]; then
    D=${DEPTH:-50}
    while :; do
      PENDING=
      while IFS= read -r pair; do
        [ -n "$pair" ] || continue
        as_hand git merge-base "$START" "${pair#* }" > /dev/null 2>&1 || {
          [ -z "$PENDING" ] || PENDING+=$'\n'
          PENDING+=$pair
        }
      done <<< "$WITH_PAIRS"
      { [ -z "$PENDING" ] || [ "$D" -ge 5000 ]; } && break
      D=$((D * 2)); [ "$D" -le 5000 ] || D=5000
      as_hand git fetch -q --depth "$D" "$URL" "$START" || finish fetch_failed
      rm -f "$RD/.git/FETCH_HEAD"
      while IFS= read -r pair; do
        [ -n "$pair" ] || continue
        as_hand git fetch -q --depth "$D" "$URL" "+${pair%% *}:${pair#* }" || finish fetch_failed
        rm -f "$RD/.git/FETCH_HEAD"
      done <<< "$PENDING"
    done
    WITH_BASE=
    i=0
    while IFS= read -r pair; do
      [ -n "$pair" ] || continue
      i=$((i + 1))
      base=$(as_hand git merge-base "$START" "${pair#* }" 2> /dev/null) || base=none
      [ -z "$WITH_BASE" ] || WITH_BASE+=$'\n'
      WITH_BASE+="with_${i}_merge_base=$base"
    done <<< "$WITH_PAIRS"
  fi
  if [ -n "$WITH_EXTRA" ]; then
    RESULT_EXTRA="with_count=$WITH_N"$'\n'"$WITH_EXTRA"
    if [ -z "${LOCAL_ACTION:-}" ]; then
      RESULT_EXTRA+=$'\n'"fetch_depth=$D"$'\n'"$WITH_BASE"
    fi
  fi
fi
echo "from $FROM at ${START:0:12} prepared_s $(( $(now) - T0 ))"

mkdir -p "$AGENT_HOME" && cp "$J/brief.md" "$J/rules.md" "$AGENT_HOME/" || finish workdir_missing
[ ! -f "$J/mcp.json" ] || cp "$J/mcp.json" "$AGENT_HOME/" || finish workdir_missing
HAND_FILES="$AGENT_HOME/brief.md $AGENT_HOME/rules.md"
[ ! -f "$AGENT_HOME/mcp.json" ] || HAND_FILES="$HAND_FILES $AGENT_HOME/mcp.json"
[ -n "${UNPRIVILEGED:-}" ] || chown hand:hand $HAND_FILES
HARNESS_CMD=$(cat "$J/harness.cmd")
HARNESS_CMD=${HARNESS_CMD//\/home\/hand\//$AGENT_HOME/}

# Clean environment: HOME, PATH, and only the names the config lists. The
# pool's commit identity travels under its Git names, so `git merge` and
# `git commit` work unconfigured; it is not a secret.
HARNESS_ENV=("HOME=$AGENT_USER_HOME" "PATH=$PATH" "VIVI_AGENT_JOB=$JOB")
HARNESS_ENV+=("GIT_AUTHOR_NAME=$COMMIT_NAME" "GIT_AUTHOR_EMAIL=$COMMIT_EMAIL")
HARNESS_ENV+=("GIT_COMMITTER_NAME=$COMMIT_NAME" "GIT_COMMITTER_EMAIL=$COMMIT_EMAIL")
[ -z "${VIVI_SLOT_ROOT:-}" ] || HARNESS_ENV+=("VIVI_SLOT_ROOT=$VIVI_SLOT_ROOT")
for name in $ENV_NAMES; do
  [ -n "${!name+x}" ] && HARNESS_ENV+=("$name=${!name}")
done
# The MCP bridge, when the job carries a grant: the agent reads its task and
# reports as the dispatching member. The facts (run_fact) keep their own
# minimal environment and do not get these.
[ -z "$BRIDGE_KEY" ] || HARNESS_ENV+=(
  "VIVI_MCP_API_KEY=$BRIDGE_KEY"
  "VIVI_MCP_SERVER=$BRIDGE_SERVER"
  "VIVI_MCP_PROJECT_ID=$BRIDGE_PROJECT"
)

# Facts: each command of $J/facts.cmd (one per line) runs in the checkout as the
# agent's user, with a clean environment, before the harness starts. Per command
# the result gets fact_<n>_cmd, fact_<n>_exit and fact_<n>_out: standard output,
# or standard error when the command failed (standard output if that is empty),
# the first 40 lines and 4096 bytes, control characters dropped, backslash and
# line break escaped so a value is one line. A command gets FACT_SECS seconds
# and may write 8 MB to each stream (it is stopped beyond that, and a stopped
# command counts as failed). A failing fact never fails the job.
FACT_SECS=10
FACTS_EXTRA=
esc() {
  printf '%s' "$1" | sed -e ':a' -e '$!{N;ba' -e '}' -e 's/\\/\\\\/g' -e 's/\n/\\n/g'
}
keep_head() { head -n 40 "$1" | head -c 4096 | tr -d '\000-\010\013-\037'; }
run_fact() { # run_fact <command>: stdout to fact.out, stderr to fact.err; returns its exit code
  ( ulimit -f 8192
    as_hand env -i "HOME=$AGENT_USER_HOME" "PATH=$PATH" "VIVI_AGENT_JOB=$JOB" \
      timeout -k 2 "$FACT_SECS" bash -c "$1" ) > "$J/fact.out" 2> "$J/fact.err" < /dev/null
}
run_facts() {
  [ -s "$J/facts.cmd" ] || return 0
  local n=0 cmd rc out
  cd "$RD" || return 0
  while IFS= read -r cmd || [ -n "$cmd" ]; do
    [ -n "$cmd" ] || continue
    n=$((n + 1))
    run_fact "$cmd"; rc=$?
    if [ "$rc" -eq 0 ] || [ ! -s "$J/fact.err" ]; then out=$(keep_head "$J/fact.out"); else out=$(keep_head "$J/fact.err"); fi
    [ -z "$FACTS_EXTRA" ] || FACTS_EXTRA+=$'\n'
    FACTS_EXTRA+="fact_${n}_cmd=$(esc "$cmd")"$'\n'"fact_${n}_exit=$rc"$'\n'"fact_${n}_out=$(esc "$out")"
  done < "$J/facts.cmd"
  rm -f "$J/fact.out" "$J/fact.err"
  kill_agent
}
run_facts
A0=$(now)
as_hand env -i "${HARNESS_ENV[@]}" timeout -k 15 "$BUDGET" bash -c "$HARNESS_CMD" \
  > "$J/agent.out" 2> "$J/agent.err"
AGENT_RC=$?
AGENT_S=$(( $(now) - A0 ))
echo "agent_rc $AGENT_RC agent_s $AGENT_S"

# Nothing the agent started may outlive it into the commit and push below.
kill_agent
EXCLUDE=(.)
[ -z "${BUILD_DIR:-}" ] || EXCLUDE+=(":(exclude)$BUILD_DIR")

# A read-only job ends here, whatever the agent did: nothing is committed or
# pushed. Detaching at START with -f restores the tracked files without moving
# any branch (a branch the agent made or checked out is left as it is).
READ_ONLY_LIST_CAP=50
finish_read_only() {
  local paths count i=0 path
  paths=$( { as_hand git diff --name-only "$START" -- "${EXCLUDE[@]}"
             as_hand git ls-files --others --exclude-standard -- "${EXCLUDE[@]}"; } | sort -u )
  as_hand git checkout -q -f --detach "$START"
  as_hand git clean "${CLEAN[@]}"
  [ -n "$paths" ] || finish read_only_unchanged
  count=$(printf '%s\n' "$paths" | grep -c .)
  RESULT_EXTRA="discarded_count=$count"
  while [ "$i" -lt "$READ_ONLY_LIST_CAP" ] && IFS= read -r path; do
    i=$((i + 1))
    RESULT_EXTRA+=$'\n'"discarded_path_$i=$path"
  done <<< "$paths"
  finish read_only_discarded
}
[ -z "${READ_ONLY:-}" ] || finish_read_only
# Written jobs only from here (a read-only job never commits, so nothing
# applies). Files the pool lists in `restore` come back to START before
# anything is committed: a build that rewrites a tracked file (a lock file,
# say) must not publish the rewrite. A path is restored only when it is
# tracked at START and the tree or the index differs from START for it; a
# path that is not tracked at START is never touched, and an unchanged one
# is never listed. The list is the pool's, never a guess.
RESTORED=
RESTORE_EXTRA=
RESTORE_LIST_CAP=50
if [ -f "$J/restore.list" ]; then
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    if as_hand git cat-file -e "$START:$path" 2>/dev/null &&
       ! as_hand git diff --quiet "$START" -- "$path" 2>/dev/null; then
      as_hand git checkout -q "$START" -- "$path"
      [ -z "$RESTORED" ] || RESTORED+=$'\n'
      RESTORED+=$path
    fi
  done < "$J/restore.list"
fi
if [ -n "$RESTORED" ]; then
  count=$(printf '%s\n' "$RESTORED" | grep -c .)
  RESTORE_EXTRA="restored_count=$count"
  i=0
  while [ "$i" -lt "$RESTORE_LIST_CAP" ] && IFS= read -r path; do
    i=$((i + 1))
    RESTORE_EXTRA+=$'\n'"restored_path_$i=$path"
  done <<< "$RESTORED"
fi
CHANGED=$(as_hand git status --porcelain -- "${EXCLUDE[@]}")
# The agent may have committed on its own: HEAD then moved off START. Fetch
# mode counts this too -- the push below is what publishes the agent's own
# commits, a merge of an extra ref with both parents included. A read-only
# job never reaches this point.
MOVED=
[ -n "${READ_ONLY:-}" ] || [ "$(as_hand git rev-parse HEAD)" = "$START" ] || MOVED=1
if [ -z "$CHANGED" ] && [ -z "$MOVED" ]; then
  finish no_changes
fi
[ "${LOCAL_ACTION:-}" != create ] || as_hand git checkout -q -b "$BRANCH" || finish branch_exists
# Whatever the agent left is kept, whatever its exit code: a crash, a refusal
# or the budget timeout (exit 124) still commits the leftover work, and a
# failed attempt says so in its commit subject. When the failed agent left
# nothing uncommitted but made commits of its own, an empty marker commit puts
# the same subject on the branch tip. The branch is published as it is; the
# pool's caller decides what a failed attempt means.
COMMIT_SUBJECT=$SUBJECT
COMMIT_ALLOW=()
if [ "${AGENT_RC:-0}" != 0 ]; then
  COMMIT_SUBJECT="WIP (attempt failed): $SUBJECT"
  COMMIT_ALLOW=(--allow-empty)
fi
if [ -n "$CHANGED" ] || [ "${AGENT_RC:-0}" != 0 ]; then
  TRAILER="Pool-Run: slot=$SLOT model=$MODEL from=${START:0:10} exit=$AGENT_RC duration=${AGENT_S}s"
  as_hand git add -A && { [ -z "${BUILD_DIR:-}" ] || as_hand git reset -q -- "$BUILD_DIR"; } \
    && as_hand git -c user.name="$COMMIT_NAME" -c user.email="$COMMIT_EMAIL" \
         commit -q -m "$COMMIT_SUBJECT" -m "$TRAILER" "${COMMIT_ALLOW[@]}" || finish commit_failed
fi
if [ -n "${LOCAL_ACTION:-}" ]; then
  finish committed "$(as_hand git rev-parse HEAD)"
fi
if as_hand git push -q "$URL" "HEAD:refs/heads/$BRANCH"; then
  finish pushed "$(as_hand git rev-parse HEAD)"
fi
finish push_refused
