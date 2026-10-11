#!/bin/bash
# Vivi pool job runner. Runs inside a slot as root (or, where UNPRIVILEGED is
# set, as the ordinary user that owns the slot), started by the boot script
# under a hard time limit. Everything it needs is in $WORK/job (default
# /work/job), delivered as one archive:
#   params.env   REMOTE FROM BRANCH DEPTH BUDGET MODEL SLOT SUBJECT JOB TASK
#                COMMIT_NAME COMMIT_EMAIL ENV_NAMES
#                (REMOTE is the full Git address; https gets the token below,
#                a file:// address is used as given)
#   token        Git token, empty for a file:// remote; root-only, never
#                copied to the agent
#   harness.cmd  the rendered harness command
#   brief.md     the task body        rules.md  the standing rules
# Environment from the machine: BUILD_DIR, and any variable named in
# ENV_NAMES (their values come from the machine's own environment).
# UNPRIVILEGED, BUILD_PERSISTS and WORK are the ones of the boot script.
#
# Start from FROM (a commit or a branch), run the harness as the unprivileged
# user `hand` (the current user when UNPRIVILEGED) under BUDGET seconds, then
# commit what changed and push to BRANCH without force. The outcome goes to
# $WORK/job/result as key=value lines.
#
# Where UNPRIVILEGED is set the agent is the same user as this script and
# there is no `hand` home: the brief and the rules go to $WORK/home (the
# harness command's /home/hand/ paths are rewritten to it), the agent keeps the
# user's own HOME and PATH, the Git token file is removed as soon as it is read,
# and whatever the agent left running is found by the VIVI_AGENT_JOB variable
# that only its environment carries.
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

finish() { # finish <push-outcome> [sha]
  {
    echo "job=$JOB"
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
  } > "$J/result.partial"
  mv "$J/result.partial" "$J/result"
  exit 0
}

cd "$RD" || finish workdir_missing
TOKEN=$(cat "$J/token")
[ -z "${UNPRIVILEGED:-}" ] || rm -f "$J/token"
URL=$REMOTE
case "$REMOTE" in
  https://*) [ -n "$TOKEN" ] && URL="https://x-access-token:${TOKEN}@${REMOTE#https://}" ;;
esac

# A reused slot holds the last task's branch and stray files: return to a
# clean tree at FROM before anything runs.
as_hand git fetch -q --depth "${DEPTH:-1}" "$URL" "$FROM" || finish fetch_failed
START=$(as_hand git rev-parse FETCH_HEAD)
CLEAN=(-fdq)
[ -z "${BUILD_PERSISTS:-}" ] || CLEAN+=(-e "/$BUILD_DIR")
as_hand git checkout -q -f "$START" && as_hand git clean "${CLEAN[@]}" || finish checkout_failed
as_hand git checkout -q -B "$BRANCH" || finish checkout_failed
rm -f "$RD/.git/FETCH_HEAD"
echo "from $FROM at ${START:0:12} prepared_s $(( $(now) - T0 ))"

mkdir -p "$AGENT_HOME" && cp "$J/brief.md" "$J/rules.md" "$AGENT_HOME/" || finish workdir_missing
[ -n "${UNPRIVILEGED:-}" ] || chown hand:hand "$AGENT_HOME/brief.md" "$AGENT_HOME/rules.md"
HARNESS_CMD=$(cat "$J/harness.cmd")
HARNESS_CMD=${HARNESS_CMD//\/home\/hand\//$AGENT_HOME/}

# Clean environment: HOME, PATH, and only the names the config lists.
HARNESS_ENV=("HOME=$AGENT_USER_HOME" "PATH=$PATH" "VIVI_AGENT_JOB=$JOB")
[ -z "${VIVI_SLOT_ROOT:-}" ] || HARNESS_ENV+=("VIVI_SLOT_ROOT=$VIVI_SLOT_ROOT")
for name in $ENV_NAMES; do
  [ -n "${!name+x}" ] && HARNESS_ENV+=("$name=${!name}")
done
A0=$(now)
as_hand env -i "${HARNESS_ENV[@]}" timeout -k 15 "$BUDGET" bash -c "$HARNESS_CMD" \
  > "$J/agent.out" 2> "$J/agent.err"
AGENT_RC=$?
AGENT_S=$(( $(now) - A0 ))
echo "agent_rc $AGENT_RC agent_s $AGENT_S"

# Nothing the agent started may outlive it into the commit and push below.
kill_agent
EXCLUDE=(. ":(exclude)$BUILD_DIR")
if [ -z "$(as_hand git status --porcelain -- "${EXCLUDE[@]}")" ]; then
  finish no_changes
fi
TRAILER="Pool-Run: slot=$SLOT model=$MODEL from=${START:0:10} exit=$AGENT_RC duration=${AGENT_S}s"
as_hand git add -A && as_hand git reset -q -- "$BUILD_DIR" \
  && as_hand git -c user.name="$COMMIT_NAME" -c user.email="$COMMIT_EMAIL" \
       commit -q -m "$SUBJECT" -m "$TRAILER" || finish commit_failed
if as_hand git push -q "$URL" "HEAD:refs/heads/$BRANCH"; then
  finish pushed "$(as_hand git rev-parse HEAD)"
fi
finish push_refused
