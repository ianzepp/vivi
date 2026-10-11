#!/bin/bash
# Vivi pool job runner. Runs inside a slot as root, started by the boot script
# under a hard time limit. Everything it needs is in /work/job, delivered as
# one archive:
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
#
# Start from FROM (a commit or a branch), run the harness as the unprivileged
# user `hand` under BUDGET seconds, then commit what changed and push to BRANCH
# without force. The outcome goes to /work/job/result as key=value lines.
J=/work/job
RD=/work/repo
. "$J/params.env"
now() { date +%s; }
T0=$(now)
START=
AGENT_RC=
AGENT_S=
as_hand() { runuser -u hand -- "$@"; }

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
URL=$REMOTE
case "$REMOTE" in
  https://*) [ -n "$TOKEN" ] && URL="https://x-access-token:${TOKEN}@${REMOTE#https://}" ;;
esac

# A reused slot holds the last task's branch and stray files: return to a
# clean tree at FROM before anything runs.
as_hand git fetch -q --depth "${DEPTH:-1}" "$URL" "$FROM" || finish fetch_failed
START=$(as_hand git rev-parse FETCH_HEAD)
as_hand git checkout -q -f "$START" && as_hand git clean -fdq || finish checkout_failed
as_hand git checkout -q -B "$BRANCH" || finish checkout_failed
rm -f "$RD/.git/FETCH_HEAD"
echo "from $FROM at ${START:0:12} prepared_s $(( $(now) - T0 ))"

cp "$J/brief.md" "$J/rules.md" /home/hand/ && chown hand:hand /home/hand/brief.md /home/hand/rules.md

# Clean environment: HOME, PATH, and only the names the config lists.
HARNESS_ENV=("HOME=/home/hand" "PATH=$PATH")
for name in $ENV_NAMES; do
  [ -n "${!name+x}" ] && HARNESS_ENV+=("$name=${!name}")
done
A0=$(now)
as_hand env -i "${HARNESS_ENV[@]}" timeout -k 15 "$BUDGET" bash -c "$(cat "$J/harness.cmd")" \
  > "$J/agent.out" 2> "$J/agent.err"
AGENT_RC=$?
AGENT_S=$(( $(now) - A0 ))
echo "agent_rc $AGENT_RC agent_s $AGENT_S"

# Nothing the agent started may outlive it into the commit and push below.
pkill -KILL -u hand 2>/dev/null
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
