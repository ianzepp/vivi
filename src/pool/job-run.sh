#!/bin/bash
# Vivi pool job runner. Runs inside a slot as root (or, where UNPRIVILEGED is
# set, as the ordinary user that owns the slot), started by the boot script
# under a hard time limit. Everything it needs is in $WORK/job (default
# /work/job), delivered as one archive:
#   params.env   REMOTE FROM BRANCH DEPTH BUDGET MODEL SLOT SUBJECT JOB TASK
#                COMMIT_NAME COMMIT_EMAIL ENV_NAMES RAMDISK_SIZE_GIB
#                RAMDISK_ENV_COUNT and numbered RAMDISK_ENV_<n>_NAME/VALUE
#                READ_ONLY (set to 1 for a read-only job; BRANCH is then empty)
#                (REMOTE is the full Git address; https gets the token below,
#                a file:// address is used as given). A worktree slot has no
#                REMOTE and also gets LOCAL_ACTION (create | continue) and
#                LOCAL_START (the commit FROM resolved to). With a bridge
#                grant also BRIDGE_SERVER and BRIDGE_PROJECT (not secret).
#                PUBLISH_REF, PUBLISH_EXPECT and PUBLISH_REMOTE are runner-only:
#                after agent exit 0, the runner may publish HEAD to the exact
#                heads ref and optionally require HEAD to equal the expected SHA.
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
# A privileged publish ref is never added to the agent environment. After a
# clean exit the runner pushes the exact checked HEAD with a plain push, then
# reads the remote ref back and records its full SHA. An expected SHA prevents
# promotion from publishing a changed candidate. Local worktree jobs publish
# through the configured remote without updating a local checked-out branch.
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
PUBLISH_STATE=
PUBLISH_CANDIDATE=
PUBLISH_SHA=
PUBLISH_CONTAINS_VERIFIED=0
DELETE_STATE=
DELETE_BEFORE=
DELETE_AFTER=
PUBLISH_DIR=
PUBLISH_REPO=
PUBLISH_HOME=
PUBLISH_GIT=
PUBLISH_PATH=
PUBLISH_ASKPASS=
if [ -n "${UNPRIVILEGED:-}" ]; then
  as_hand() { "$@"; }
  AGENT_HOME=$W/home
  AGENT_USER_HOME=${HOME:-$AGENT_HOME}
  agent_job_pids() {
    grep -lzxF "VIVI_AGENT_JOB=$JOB" /proc/[0-9]*/environ 2>/dev/null | sed 's#/proc/\([0-9]*\)/environ#\1#'
  }
  no_agent_running() { [ -z "$(agent_job_pids)" ]; }
  kill_agent() {
    local pids
    pids=$(agent_job_pids)
    [ -z "$pids" ] || kill -KILL $pids 2>/dev/null
    wait_for_agent_exit
  }
else
  as_hand() { runuser -u hand -- "$@"; }
  AGENT_HOME=/home/hand
  AGENT_USER_HOME=/home/hand
  no_agent_running() { ! pgrep -u hand >/dev/null 2>&1; }
  kill_agent() {
    pkill -KILL -u hand 2>/dev/null || true
    wait_for_agent_exit
  }
fi

wait_for_agent_exit() {
  local attempt
  for ((attempt = 0; attempt < 20; attempt++)); do
    no_agent_running && return 0
    sleep 0.05
  done
  no_agent_running
}

clean_publish_url() {
  local url=$1 scheme remainder authority suffix=
  case "$url" in
    https://*|http://*)
      scheme=${url%%://*}
      remainder=${url#*://}
      authority=${remainder%%/*}
      [ "$authority" = "$remainder" ] || suffix=/${remainder#*/}
      authority=${authority##*@}
      printf '%s://%s%s' "$scheme" "$authority" "$suffix"
      ;;
    *) printf '%s' "$url" ;;
  esac
}

valid_git_oid() {
  case "$1" in ''|*[!0-9a-f]*) return 1 ;; esac
  [ "${#1}" -eq 40 ] || [ "${#1}" -eq 64 ]
}

hand_git() {
  as_hand env -i "HOME=$AGENT_USER_HOME" "PATH=$PATH" \
    GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_GLOBAL=/dev/null \
    GIT_TERMINAL_PROMPT=0 git -c core.hooksPath=/dev/null -C "$RD" "$@"
}

publisher_env_git() {
  no_agent_running || return 90
  env -i "HOME=$PUBLISH_HOME" "PATH=$PUBLISH_PATH" \
    GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_GLOBAL=/dev/null \
    GIT_CONFIG_COUNT=0 GIT_TERMINAL_PROMPT=0 \
    GIT_ASKPASS="$PUBLISH_ASKPASS" "VIVI_PUBLISH_TOKEN=${TOKEN:-}" \
    "$PUBLISH_GIT" -c core.hooksPath=/dev/null "$@"
}

publisher_git() {
  publisher_env_git --git-dir="$PUBLISH_REPO" "$@"
}

init_publisher() {
  [ -n "$PUBLISH_REPO" ] && return 0
  no_agent_running || return 1
  PUBLISH_GIT=/usr/bin/git
  [ -x "$PUBLISH_GIT" ] || return 1
  PUBLISH_PATH=/usr/bin:/bin:/usr/sbin:/sbin:/usr/lib/git-core:/usr/libexec/git-core
  PUBLISH_DIR=$(mktemp -d "$W/publisher.XXXXXXXX") || return 1
  chmod 700 "$PUBLISH_DIR" || return 1
  PUBLISH_HOME=$PUBLISH_DIR/home
  PUBLISH_REPO=$PUBLISH_DIR/repo.git
  PUBLISH_ASKPASS=$PUBLISH_DIR/askpass
  mkdir -m 700 "$PUBLISH_HOME" "$PUBLISH_DIR/template" || return 1
  cat > "$PUBLISH_ASKPASS" <<'ASKPASS'
#!/bin/bash
case "$1" in
  *Username*) printf '%s\n' x-access-token ;;
  *Password*) printf '%s\n' "${VIVI_PUBLISH_TOKEN:-}" ;;
  *) exit 1 ;;
esac
ASKPASS
  chmod 700 "$PUBLISH_ASKPASS" || return 1
  PUBLISH_OBJECTS=$(hand_git rev-parse --path-format=absolute --git-path objects) || return 1
  PUBLISH_SHALLOW=$(hand_git rev-parse --path-format=absolute --git-path shallow) || return 1
  case "$PUBLISH_OBJECTS$PUBLISH_SHALLOW" in *$'\n'*|*$'\r'*) return 1 ;; esac
  case "$PUBLISH_OBJECTS" in /*) ;; *) return 1 ;; esac
  [ -d "$PUBLISH_OBJECTS" ] || return 1
  publisher_env_git init --bare --template="$PUBLISH_DIR/template" "$PUBLISH_REPO" || return 1
  mkdir -p "$PUBLISH_REPO/objects/info" || return 1
  printf '%s\n' "$PUBLISH_OBJECTS" > "$PUBLISH_REPO/objects/info/alternates" || return 1
  if [ -e "$PUBLISH_SHALLOW" ]; then
    [ -f "$PUBLISH_SHALLOW" ] && [ ! -L "$PUBLISH_SHALLOW" ] || return 1
    : > "$PUBLISH_REPO/shallow" || return 1
    while IFS= read -r shallow || [ -n "$shallow" ]; do
      [ -n "$shallow" ] || continue
      valid_git_oid "$shallow" || return 1
      printf '%s\n' "$shallow" >> "$PUBLISH_REPO/shallow" || return 1
    done < "$PUBLISH_SHALLOW"
  fi
}

# A worktree slot at rest is detached, so the branch it worked on is free to
# be checked out or merged elsewhere. Never touches the working tree.
detach_local() {
  [ -z "${LOCAL_ACTION:-}" ] || as_hand git -C "$RD" checkout -q --detach 2>/dev/null
  return 0
}
[ -z "${LOCAL_ACTION:-}" ] || trap detach_local EXIT
[ -z "${LOCAL_ACTION:-}" ] || trap 'exit 143' TERM HUP INT

RAMDISK_FULL_DETECTED=0
ramdisk_full_output() {
  grep -Eq 'No space left on device|os error 28' "$@" 2>/dev/null
}

ramdisk_full() { # ramdisk_full <push-outcome>
  [[ "${RAMDISK_SIZE_GIB:-0}" =~ ^[1-9][0-9]*$ ]] || return 1
  [ "$RAMDISK_FULL_DETECTED" -eq 1 ] && return 0
  if [ -n "${AGENT_RC:-}" ] && [ "$AGENT_RC" -ne 0 ]; then
    ramdisk_full_output "$J/agent.out" "$J/agent.err" "$J/run.log"
    return $?
  fi
  case "$1" in
    pushed|committed|no_changes|read_only_*) return 1 ;;
  esac
  ramdisk_full_output "$J/agent.out" "$J/agent.err" "$J/run.log"
}

finish() { # finish <push-outcome> [sha]
  local push_word=$1 sha=${2:-}
  if ramdisk_full "$push_word"; then
    push_word=ramdisk_full
    sha=
    AGENT_RC=${AGENT_RC:-1}
    RESULT_EXTRA="ramdisk_size_gib=$RAMDISK_SIZE_GIB"
  fi
  detach_local
  {
    echo "job=$JOB"
    [ -z "${TARGET:-}" ] || echo "target=$TARGET"
    [ -z "${REPOSITORY:-}" ] || echo "repository=$REPOSITORY"
    echo "state=done"
    echo "push=$push_word"
    echo "branch=$BRANCH"
    echo "sha=$sha"
    echo "start=$START"
    echo "agent_rc=$AGENT_RC"
    echo "agent_s=$AGENT_S"
    echo "job_s=$(( $(now) - T0 ))"
    echo "model=$MODEL"
    echo "slot=$SLOT"
    if [ -n "${PUBLISH_REF:-}" ]; then
      echo "publish_state=${PUBLISH_STATE:-not-attempted}"
      echo "publish_ref=$PUBLISH_REF"
      echo "publish_candidate=${PUBLISH_CANDIDATE:--}"
      echo "publish_sha=${PUBLISH_SHA:--}"
      echo "publish_contains=${PUBLISH_CONTAINS:-}"
      echo "publish_contains_verified=$PUBLISH_CONTAINS_VERIFIED"
    fi
    if [ -n "${DELETE_REF:-}" ]; then
      echo "delete_state=${DELETE_STATE:-not-attempted}"
      echo "delete_ref=$DELETE_REF"
      echo "delete_expect=${DELETE_EXPECT:-}"
      echo "delete_before=${DELETE_BEFORE:--}"
      echo "delete_after=${DELETE_AFTER:--}"
    fi
    [ -z "${RESULT_EXTRA:-}" ] || printf '%s\n' "$RESULT_EXTRA"
    [ -z "${RESTORE_EXTRA:-}" ] || printf '%s\n' "$RESTORE_EXTRA"
    [ -z "${FACTS_EXTRA:-}" ] || printf '%s\n' "$FACTS_EXTRA"
  } > "$J/result.partial"
  mv "$J/result.partial" "$J/result"
  [ -z "$PUBLISH_DIR" ] || rm -rf -- "$PUBLISH_DIR"
  exit 0
}

# Import a hand-verified object into the isolated runner repo, then push its
# exact id. Git never reads the hand checkout's config or hooks here.
publish_candidate_to() {
  local url=$1 ref=$2 candidate=$3 allow_existing=$4 push_state remote_line remote_sha remote_ref
  PUBLISH_CANDIDATE=$candidate
  valid_git_oid "$candidate" || { PUBLISH_STATE=candidate_missing; return 1; }
  case "$ref" in refs/heads/*) ;; *) PUBLISH_STATE=invalid_ref; return 1 ;; esac
  init_publisher || { PUBLISH_STATE=publisher_unavailable; return 1; }
  publisher_git check-ref-format "$ref" >/dev/null 2>&1 || {
    PUBLISH_STATE=invalid_ref
    return 1
  }
  publisher_git cat-file -e "$candidate^{commit}" 2>/dev/null || {
    PUBLISH_STATE=candidate_missing
    return 1
  }
  PUBLISH_CONTAINS_VERIFIED=0
  if [ -n "${PUBLISH_CONTAINS:-}" ]; then
    valid_git_oid "$PUBLISH_CONTAINS" && publisher_git merge-base --is-ancestor "$PUBLISH_CONTAINS" "$candidate" || {
      PUBLISH_STATE=required_source_missing
      return 1
    }
  fi
  if [ -n "${PUBLISH_EXPECT:-}" ] && [[ "$ref" == refs/heads/vivi/need/* ]]; then
    guarded_need_create "$url" "$ref" "$candidate" || return 1
    push_state=$GUARDED_PUSH_STATE
  elif publisher_git push --no-verify -q "$url" "$candidate:$ref"; then
    push_state=pushed
  elif [ "$allow_existing" = yes ]; then
    push_state=already-on-remote
  else
    PUBLISH_STATE=push_refused
    return 1
  fi
  publisher_git fetch -q --no-tags "$url" "$ref" 2>/dev/null || {
    PUBLISH_STATE=verify_failed
    return 1
  }
  PUBLISH_SHA=$(publisher_git rev-parse --verify --quiet 'FETCH_HEAD^{commit}' 2>/dev/null) || {
    PUBLISH_STATE=verify_failed
    return 1
  }
  remote_line=$(publisher_git ls-remote --heads "$url" "$ref" 2>/dev/null) || {
    PUBLISH_STATE=verify_failed
    return 1
  }
  read -r remote_sha remote_ref <<< "$remote_line"
  if [ "$remote_ref" != "$ref" ] || [ "$remote_sha" != "$PUBLISH_SHA" ] || ! valid_git_oid "$remote_sha"; then
    PUBLISH_STATE=verify_failed
    PUBLISH_SHA=
    return 1
  fi
  if publisher_git merge-base --is-ancestor "$candidate" "$PUBLISH_SHA"; then
    if [ -n "${PUBLISH_CONTAINS:-}" ]; then
      publisher_git merge-base --is-ancestor "$PUBLISH_CONTAINS" "$PUBLISH_SHA" || {
        PUBLISH_STATE=required_source_missing
        return 1
      }
      PUBLISH_CONTAINS_VERIFIED=1
    fi
    PUBLISH_STATE=$push_state
    return 0
  fi
  PUBLISH_STATE=not_ancestor
  PUBLISH_SHA=
  return 1
}

# An exact-candidate expectation on an owned need ref is a create-only grant.
# A same-base replay is safe; no other existing tip is adopted or overwritten.
guarded_need_create() {
  local url=$1 ref=$2 candidate=$3 line observed observed_ref
  PUBLISH_STATE=candidate_mismatch
  [ "$candidate" = "$PUBLISH_EXPECT" ] || return 1
  PUBLISH_STATE=verify_failed
  line=$(publisher_git ls-remote --heads "$url" "$ref") || return 1
  if [ -n "$line" ]; then
    read -r observed observed_ref <<< "$line"
    if [ "$observed" = "$candidate" ] && [ "$observed_ref" = "$ref" ]; then
      GUARDED_PUSH_STATE=already-on-remote
      return 0
    fi
    PUBLISH_STATE=ref_mismatch
    return 1
  fi
  if publisher_git push --no-verify -q --force-with-lease="$ref:" "$url" "$candidate:$ref"; then
    GUARDED_PUSH_STATE=pushed
    return 0
  fi
  line=$(publisher_git ls-remote --heads "$url" "$ref") || return 1
  read -r observed observed_ref <<< "$line"
  if [ "$observed" = "$candidate" ] && [ "$observed_ref" = "$ref" ]; then
    GUARDED_PUSH_STATE=already-on-remote
    return 0
  fi
  PUBLISH_STATE=ref_mismatch
  return 1
}

# Publish a verified candidate with the runner's credentials, never from the
# agent environment. A plain push is fast-forward-only; remote ancestry is
# read back before the result claims publication.
publish_target() {
  PUBLISH_CONTAINS_VERIFIED=0
  [ -n "${PUBLISH_REF:-}" ] || return 0
  if [ "${AGENT_RC:-1}" != 0 ]; then
    PUBLISH_STATE=agent_failed
    return 0
  fi
  case "$PUBLISH_REF" in refs/heads/*) ;; *) PUBLISH_STATE=invalid_ref; return 0 ;; esac
  PUBLISH_CANDIDATE=$(hand_git rev-parse --verify 'HEAD^{commit}' 2>/dev/null) || {
    PUBLISH_STATE=candidate_missing
    return 0
  }
  valid_git_oid "$PUBLISH_CANDIDATE" || { PUBLISH_STATE=candidate_missing; return 0; }
  if [ -n "${PUBLISH_EXPECT:-}" ] && [ "$PUBLISH_CANDIDATE" != "$PUBLISH_EXPECT" ]; then
    PUBLISH_STATE=candidate_mismatch
    return 0
  fi
  PUBLISH_URL=$(clean_publish_url "${PUBLISH_REMOTE:-$REMOTE}")
  if [ -z "$PUBLISH_URL" ]; then
    PUBLISH_STATE=remote_missing
    return 0
  fi
  publish_candidate_to "$PUBLISH_URL" "$PUBLISH_REF" "$PUBLISH_CANDIDATE" yes || true
}

# Delete only this job's owned need ref, guarded by its exact old tip.
# The lease is a compare-and-delete guard; no nonempty refspec is forced.
delete_target() {
  [ -n "${DELETE_REF:-}" ] || return 0
  DELETE_STATE=agent_failed
  [ "${AGENT_RC:-1}" = 0 ] || return 0
  DELETE_STATE=invalid_ref
  case "$DELETE_REF" in refs/heads/vivi/need/*) ;; *) return 0 ;; esac
  local need_id=${DELETE_REF#refs/heads/vivi/need/} current candidate url
  case "$need_id" in ''|*/*) return 0 ;; esac
  valid_git_oid "${DELETE_EXPECT:-}" || return 0
  init_publisher || { DELETE_STATE=verify_failed; return 0; }
  publisher_git check-ref-format "$DELETE_REF" >/dev/null 2>&1 || return 0
  candidate=$(hand_git rev-parse --verify 'HEAD^{commit}') || { DELETE_STATE=verify_failed; return 0; }
  valid_git_oid "$candidate" && publisher_git merge-base --is-ancestor "$DELETE_EXPECT" "$candidate" || {
    DELETE_STATE=verify_failed
    return 0
  }
  url=$(clean_publish_url "${PUBLISH_REMOTE:-$REMOTE}")
  [ -n "$url" ] || { DELETE_STATE=verify_failed; return 0; }
  current=$(publisher_git ls-remote --heads "$url" "$DELETE_REF") || { DELETE_STATE=verify_failed; return 0; }
  if [ -z "$current" ]; then
    DELETE_BEFORE=absent
    DELETE_AFTER=absent
    DELETE_STATE=already-absent
    return 0
  fi
  read -r DELETE_BEFORE current <<< "$current"
  DELETE_STATE=ref_mismatch
  [ "$current" = "$DELETE_REF" ] && [ "$DELETE_BEFORE" = "$DELETE_EXPECT" ] || return 0
  DELETE_STATE=verify_failed
  publisher_git push -q --force-with-lease="$DELETE_REF:$DELETE_EXPECT" "$url" ":$DELETE_REF" || return 0
  current=$(publisher_git ls-remote --heads "$url" "$DELETE_REF") || return 0
  [ -z "$current" ] || return 0
  DELETE_AFTER=absent
  DELETE_STATE=deleted
}

finalize_targets() {
  publish_target
  delete_target
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

# A stale index.lock in the checkout (a slot killed mid-checkout) makes every
# later job die checkout_failed. Remove it, but never next to a live Git
# process working on this checkout: `git -C <checkout>` in some command line
# means a checkout may still be in flight, and the lock is left alone (the job
# then still ends checkout_failed, which the pump counts host-class).
# `git rev-parse --git-path index.lock` answers the worktree's own gitdir lock
# for a worktree slot and .git/index.lock for a clone slot, so one block covers
# both. pgrep excludes itself.
GD=$(as_hand git -C "$RD" rev-parse --git-path index.lock 2>/dev/null) || GD=
case "$GD" in /*) ;; *) GD="$RD/${GD:-.git/index.lock}" ;; esac
if [ -n "${GD:-}" ] && [ -e "$GD" ] && ! pgrep -f -- "$RD" >/dev/null 2>&1; then
  rm -f -- "$GD" && echo "removed stale lock $GD"
fi

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
  # The job directory holds root-only credentials; pass the script through
  # stdin rather than giving hand a path it cannot open.
  [ ! -f "$J/target-seed.sh" ] || as_hand bash < "$J/target-seed.sh" || finish target_seed_failed
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
for ((i=0; i<${RAMDISK_ENV_COUNT:-0}; i++)); do
  name_var=RAMDISK_ENV_${i}_NAME
  value_var=RAMDISK_ENV_${i}_VALUE
  name=${!name_var}
  [[ "$name" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || finish ramdisk_env_invalid
  HARNESS_ENV+=("$name=${!value_var}")
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
# command counts as failed). A failing fact is only fatal when its output proves
# that the per-job RAM disk filled; otherwise it remains report-only.
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
    if [ "$rc" -ne 0 ] && [[ "${RAMDISK_SIZE_GIB:-0}" =~ ^[1-9][0-9]*$ ]] &&
       ramdisk_full_output "$J/fact.out" "$J/fact.err"; then
      RAMDISK_FULL_DETECTED=1
    fi
    if [ "$rc" -eq 0 ] || [ ! -s "$J/fact.err" ]; then out=$(keep_head "$J/fact.out"); else out=$(keep_head "$J/fact.err"); fi
    [ -z "$FACTS_EXTRA" ] || FACTS_EXTRA+=$'\n'
    FACTS_EXTRA+="fact_${n}_cmd=$(esc "$cmd")"$'\n'"fact_${n}_exit=$rc"$'\n'"fact_${n}_out=$(esc "$out")"
  done < "$J/facts.cmd"
  rm -f "$J/fact.out" "$J/fact.err"
  kill_agent
}
run_facts
[ "$RAMDISK_FULL_DETECTED" -ne 1 ] || finish ramdisk_full
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
  [ -z "${PUBLISH_REF:-}" ] && [ -z "${DELETE_REF:-}" ] && finish no_changes
  finalize_targets
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
RUNNER_SHA=$(hand_git rev-parse --verify 'HEAD^{commit}') || finish commit_failed
valid_git_oid "$RUNNER_SHA" || finish commit_failed
if [ -n "${LOCAL_ACTION:-}" ]; then
  RUNNER_PUSH=committed
elif publish_candidate_to "$(clean_publish_url "$REMOTE")" "refs/heads/$BRANCH" "$RUNNER_SHA" no; then
  RUNNER_PUSH=pushed
else
  RUNNER_PUSH=push_refused
fi
finalize_targets
finish "$RUNNER_PUSH" "$RUNNER_SHA"
