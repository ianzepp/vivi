# Vivi pool: the remote half of the `ssh` provider. Not run on its own: the
# provider sends this text, after assignments of ROOT, VERB and A1..A4, as the
# script of one `bash -c` on the host, once per call.
#
# A slot is the directory $R/<mailspace>/<pool>/slot-<n>/ ($R is ROOT with a
# leading `~/` made the login user's home). Everything about a slot sits in
# that directory:
#   marks/<name>   one small file per mark (mailspace, pool, slot, base, state,
#                  arch)
#   created        the time the slot was made, UTC, written once at creation
#   claim          exists while a task holds the slot; written whole, then
#                  hard-linked into place, so it appears complete or not at all
#   boot.pid       process id of the slot's boot wrapper (start writes it)
#   boot.start     macOS: the boot wrapper's start time (`ps -o lstart=`)
#   exit_code      exit code of the last boot that ran to its end
#   slot.env       the environment the boot script runs with (mode 600: a pod's
#                  holds the harness environment values)
#   boot.sh ...    the boot script and the files delivered with the slot
# "Running" means boot.pid names a live process whose environment carries
# VIVI_SLOT_ROOT=<the slot directory>: a reused pid is not mistaken for it. The
# tag is inherited by everything the slot starts, so stopping finds the whole
# tree, not only the process group. Linux reads /proc.
# macOS shows no process environments (macOS 27: `ps -E` prints none, and the
# old tag search matched only awk's own arguments, so no slot could be stopped
# and no job's check ever ran). There a slot's processes are its process
# groups: the boot runs under setsid, so its group id is boot.pid, and the job
# runner records each group it starts under timeout (agent, facts, check) in
# job/agent.groups. Every recorded group carries its leader's start time; a
# group whose id now names a process that started at another time is a reused
# pid and not this slot's. A process that leaves its group (setsid, setpgid)
# is not found.
#
# UMASK (077 unless the provider says otherwise) is the umask of everything the
# script writes. A host that runs the slot's scripts as root and its agent as
# another user (a pod) needs the slot directory to be traversable: it says 022.
# Secrets are made private by their own chmod, not by the umask.
#
# Verbs and their arguments:
#   list A1 A2                 the facts of A2's slots under mailspace A1
#   machine A1                 the facts of slot A1 (nothing when it is gone)
#   probe                      what `vivi pool doctor` wants to know of the host
#   envgaps A1                 the names in A1 that the login environment lacks
#   create A1                  new slot A1 from a tar stream on standard input
#   start A1 / stop A1         run the boot script detached / end everything
#   claim A1 A2 A3             take slot A1 for task A2 at time A3
#   unclaim A1                 drop the claim
#   put A1 A2 A3               write standard input to A1/A2 for claim nonce A3
#   seal A1 A2 A3              set mark A2 of slot A1 to A3
#   read A1 A2                 tar stream of the files named in A2 that exist
#   exec A1 A2                 run script A2 with the slot as WORK and cwd
#   destroy A1                 stop and delete slot A1 (guarded)
SYSTEM=$(uname -s)
umask "$UMASK"
fail() { echo "vivi-ssh: $*" >&2; exit 1; }
refuse() { echo "vivi-ssh: refusing to delete: $*" >&2; exit 92; }

case "$ROOT" in
  "~/"*) R="$HOME/${ROOT#\~/}" ;;
  *) R=$ROOT ;;
esac

# A slot id is <mailspace>/<pool>/slot-<n> and nothing else.
slot_ok() { [[ $1 =~ ^[a-z0-9][a-z0-9-]*/[a-z0-9][a-z0-9-]*/slot-[0-9]+$ ]]; }
need_slot() {
  if [ -n "${EXPLICIT_DIR:-}" ]; then D=$EXPLICIT_DIR/.vivi; return; fi
  slot_ok "$1" || fail "not a slot id: $1"
  D=$(cd "$R/$1" 2>/dev/null && pwd -P) || D="$R/$1"
}
known() { [ -f "$D/marks/vivi_pool" ]; }

# One start time, spaces collapsed, as `ps -o lstart=` prints it.
lstart_of() { ps -o lstart= -p "$1" 2>/dev/null | tr -s ' ' | sed 's/^ //; s/ $//'; }
# macOS: the members of the groups named on stdin ("id|start" lines) that are
# still the slot's. Arguments travel in the environment, never on awk's
# command line, so the search cannot match its own process.
group_members() {
  VIVI_GROUPS=$(cat) VIVI_SNAP=$(ps -A -o pid=,pgid=,lstart= 2>/dev/null) awk '
    BEGIN {
      n = split(ENVIRON["VIVI_GROUPS"], g, "\n")
      for (i = 1; i <= n; i++) {
        k = index(g[i], "|"); if (k < 2) continue
        id = substr(g[i], 1, k - 1); if (id !~ /^[0-9]+$/) continue
        s = substr(g[i], k + 1); gsub(/ +/, " ", s); sub(/^ /, "", s); sub(/ $/, "", s)
        want[id] = s
      }
      m = split(ENVIRON["VIVI_SNAP"], rows, "\n")
      for (i = 1; i <= m; i++) {
        c = split(rows[i], f, " "); if (c < 3) continue
        s = f[3]; for (j = 4; j <= c; j++) s = s " " f[j]
        start[f[1]] = s; grp[f[1]] = f[2]
      }
      for (p in grp) {
        id = grp[p]
        if (!(id in want)) continue
        if ((id in start) && want[id] != "" && start[id] != want[id]) continue
        print p
      }
    }'
}
darwin_groups() {
  local p
  p=$(cat "$1/boot.pid" 2>/dev/null)
  [[ $p =~ ^[0-9]+$ ]] && printf '%s|%s\n' "$p" "$(cat "$1/boot.start" 2>/dev/null)"
  grep -E '^[0-9]+[|]' "$1/job/agent.groups" 2>/dev/null
  return 0
}
# Every process the slot runs: by the environment tag on Linux, by process
# group on macOS.
tagged() {
  if [ "$SYSTEM" = Darwin ]; then
    darwin_groups "$1" | group_members | sort -n
  else
    grep -lzxF "VIVI_SLOT_ROOT=$1" /proc/[0-9]*/environ 2>/dev/null | sed 's#/proc/\([0-9]*\)/environ#\1#'
  fi
}
alive() {
  local p now
  p=$(cat "$1/boot.pid" 2>/dev/null) || return 1
  [[ $p =~ ^[0-9]+$ ]] || return 1
  if [ "$SYSTEM" = Darwin ]; then
    now=$(lstart_of "$p")
    [ -n "$now" ] || return 1
    if [ -s "$1/boot.start" ]; then
      [ "$now" = "$(tr -s ' ' < "$1/boot.start" | sed 's/^ //; s/ $//')" ]
      return $?
    fi
    # A slot started before boot.start was recorded: the boot is a session
    # (and group) leader, which an unrelated reuse of its pid rarely is.
    [ "$(ps -o pgid= -p "$p" 2>/dev/null | tr -d ' ')" = "$p" ]
    return $?
  fi
  [ -r "/proc/$p/environ" ] || return 1
  tr '\0' '\n' < "/proc/$p/environ" 2>/dev/null | grep -qxF "VIVI_SLOT_ROOT=$1"
}
# Everything the slot runs: the tagged processes and the boot session.
slot_pids() {
  if [ "$SYSTEM" = Darwin ]; then tagged "$1" | sort -un | tr '\n' ' '; return; fi
  {
    tagged "$1"
    local p
    p=$(cat "$1/boot.pid" 2>/dev/null)
    if [[ $p =~ ^[0-9]+$ ]] && alive "$1"; then
      ps -eo pid=,sid= 2>/dev/null | awk -v s="$p" '$2 == s { print $1 }'
    fi
  } | sort -un | tr '\n' ' '
}

emit() {
  local id=$1 d f state=stopped
  if [ -n "${EXPLICIT_DIR:-}" ]; then
    d=$EXPLICIT_DIR/.vivi
    echo "@@slot $id"
    echo "mark.vivi_pool=$EXPLICIT_POOL"
    echo "mark.vivi_pool_mailspace=$EXPLICIT_SPACE"
    echo "mark.vivi_pool_state=ready"
  else
    d=$(cd "$R/$id" && pwd -P) || return 0
    echo "@@slot $id"
  fi
  alive "$d" && state=started
  echo "state=$state"
  [ "$state" = stopped ] && [ -f "$d/exit_code" ] && echo "exit=$(head -n1 "$d/exit_code")"
  echo "created=$(head -n1 "$d/created" 2>/dev/null)"
  for f in "$d"/marks/*; do
    if [ -n "${EXPLICIT_DIR:-}" ]; then
      case "${f##*/}" in vivi_pool|vivi_pool_mailspace|vivi_pool_state|vivi_pool_slot) continue ;; esac
    fi
    [ -f "$f" ] && printf 'mark.%s=%s\n' "${f##*/}" "$(head -n1 "$f")"
  done
  if [ -f "$d/claim" ]; then
    sed -n 's/^task=/claim.task=/p; s/^claimed=/claim.claimed=/p' "$d/claim"
  fi
  return 0
}

list_slots() {
  local m id
  [ -d "$R" ] || return 0
  for m in "$R"/"$A1"/"$A2"/slot-*/marks/vivi_pool; do
    [ -f "$m" ] || continue
    id=${m#"$R"/}; id=${id%/marks/vivi_pool}
    slot_ok "$id" && emit "$id"
  done
  return 0
}

create_slot() {
  local pool part
  slot_ok "$A1" || fail "not a slot id: $A1"
  [ -e "$R/$A1" ] && fail "slot directory exists: $A1"
  [ -d "$R" ] || { mkdir -p "$R" && : > "$R/.vivi-root"; } || fail "cannot create $ROOT"
  pool=${A1%/*}
  mkdir -p "$R/$pool" || fail "cannot create $ROOT/$pool"
  part="$R/$pool/.partial-${A1##*/}.$$"
  mkdir "$part" && tar -xf - -C "$part" || { rm -rf "$part"; fail "could not unpack the new slot"; }
  # The tar stream carries no useful times, so the slot's age is recorded here.
  date -u +%Y-%m-%dT%H:%M:%SZ > "$part/created"
  [ ! -f "$part/once.env" ] || chmod 600 "$part/once.env"
  [ ! -f "$part/slot.env" ] || chmod 600 "$part/slot.env"
  case $(uname -m) in
    x86_64) echo amd64 ;;
    aarch64|arm64) echo arm64 ;;
    *) uname -m ;;
  esac > "$part/marks/vivi_pool_arch"
  mv "$part" "$R/$A1" || { rm -rf "$part"; fail "could not place the new slot"; }
}

# macOS has no setsid utility, but Perl exposes the same POSIX call.
detach() {
  if [ "$SYSTEM" != Darwin ] || command -v setsid >/dev/null 2>&1; then
    nohup setsid "$@"
  else
    nohup perl -MPOSIX -e 'POSIX::setsid() or die "setsid: $!\n"; exec @ARGV or die "exec: $!\n"' "$@"
  fi
}

start_slot() {
  local _
  need_slot "$A1"; known || fail "no such slot: $A1"
  alive "$D" && return 0
  rm -f "$D/exit_code" "$D/boot.pid" "$D/boot.start"
  (
    set -a; . "$D/slot.env"; set +a
    # A login shell may reorder PATH; the boot command puts this order back.
    export WORK="$D" VIVI_SLOT_ROOT="$D" VIVI_SESSION_PATH="$PATH"
    [ -z "${EXPLICIT_DIR:-}" ] || export SLOT_DIR="$EXPLICIT_DIR"
    cd "$D" || exit 1
    detach bash -l -c 'PATH="$VIVI_SESSION_PATH:$PATH"; set -f; q=; IFS=:; for d in $PATH; do [ -n "$d" ] || continue; case ":$q:" in *":$d:"*) ;; *) q="${q:+$q:}$d" ;; esac; done; unset IFS; set +f; PATH=$q; unset VIVI_SESSION_PATH d q; echo $$ > "$WORK/boot.pid"; bash "$WORK/boot.sh"; rc=$?; echo "$rc" > "$WORK/exit_code"; exit "$rc"' \
      > "$D/boot.out" 2>&1 < /dev/null &
  )
  for _ in $(seq 100); do
    if [ -s "$D/boot.pid" ]; then
      [ "$SYSTEM" != Darwin ] || lstart_of "$(cat "$D/boot.pid")" > "$D/boot.start"
      return 0
    fi
    sleep 0.05
  done
  fail "the slot did not start (see boot.out in $A1)"
}

stop_slot() {
  local pids sig _
  need_slot "$A1"; [ -d "$D" ] || return 0
  pids=$(slot_pids "$D")
  for sig in TERM KILL; do
    [ -n "$pids" ] || break
    kill -"$sig" $pids 2>/dev/null
    for _ in $(seq 50); do
      pids=$(slot_pids "$D")
      [ -n "$pids" ] || break
      sleep 0.1
    done
  done
  [ -z "$pids" ] || fail "could not stop processes of $A1: $pids"
  busy_release
  rm -f "$D/boot.pid" "$D/boot.start" "$D/exit_code"
}

free_now() {
  [ "$(head -n1 "$D/marks/vivi_pool_state" 2>/dev/null)" = ready ] && ! alive "$D"
}

# The claim is one file, written whole and then hard-linked to its name: `ln`
# fails if the name exists, so of two claimants on any machines exactly one
# gets it, and the winner's task and time are in place the moment it exists.
claim_slot() {
  local nonce tmp
  need_slot "$A1"
  if [ -n "${EXPLICIT_DIR:-}" ]; then
    parent=$D
    while [ "$parent" != / ]; do
      [ ! -L "$parent" ] || fail "symlinked slot directory"
      parent=${parent%/*}; [ -n "$parent" ] || parent=/
    done
    mkdir -p "$D/marks" || fail "cannot create slot records"
    printf '%s\n' ready > "$D/marks/vivi_pool_state"
    printf '%s\n' "$EXPLICIT_POOL" > "$D/marks/vivi_pool"
    printf '%s\n' "$EXPLICIT_SPACE" > "$D/marks/vivi_pool_mailspace"
  fi
  known || { echo gone; return 0; }
  nonce=$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')
  tmp="$D/claim.$$.$nonce.tmp"
  printf 'task=%s\nclaimed=%s\nnonce=%s\n' "$A2" "$A3" "$nonce" > "$tmp" || fail "cannot write the claim"
  if ln "$tmp" "$D/claim" 2>/dev/null; then
    rm -f "$tmp"
    if free_now && { rm -f "$D/boot.pid"; busy_take; }; then echo "claimed $nonce"; else rm -f "$D/claim"; echo busy; fi
  else
    rm -f "$tmp"
    [ -e "$D/claim" ] && echo taken || fail "cannot create the claim (hard links unavailable?)"
  fi
}

put_file() {
  local held
  need_slot "$A1"; known || fail "no such slot: $A1"
  held=$(sed -n 's/^nonce=//p' "$D/claim" 2>/dev/null)
  [ -n "$held" ] && [ "$held" = "$A3" ] || fail "not the claim holder of $A1"
  case "$A2" in ''|/*|*..*) fail "bad path: $A2" ;; esac
  mkdir -p "$(dirname "$D/$A2")" && ( umask 077; cat > "$D/$A2.partial" ) && mv -f "$D/$A2.partial" "$D/$A2" \
    || fail "cannot write $A2"
}

seal_mark() {
  need_slot "$A1"; known || fail "no such slot: $A1"
  [[ $A2 =~ ^[a-z_]+$ ]] || fail "bad mark name: $A2"
  printf '%s\n' "$A3" > "$D/marks/$A2.partial" && mv -f "$D/marks/$A2.partial" "$D/marks/$A2" \
    || fail "cannot write mark $A2"
}

read_files() {
  local p list=
  need_slot "$A1"; cd "$D" 2>/dev/null || return 0
  for p in $A2; do
    case "$p" in ''|/*|*..*) continue ;; esac
    [ -f "$p" ] && list="$list $p"
  done
  [ -z "$list" ] || tar -cf - $list
}

# Deleting is the one dangerous act here, so it asks four things: the id is a
# slot id; the root is a real directory of its own; the slot directory is a
# plain directory (not a link) that really lies inside the root; and it holds
# Vivi's own marks naming this pool AND this mailspace.
destroy_slot() {
  local pool space real root
  if [ -n "${EXPLICIT_DIR:-}" ]; then stop_slot; return 0; fi
  slot_ok "$A1" || refuse "not a slot id: $A1"
  case "$R" in ''|/) refuse "the root is empty or /" ;; /*) ;; *) refuse "the root is not absolute: $R" ;; esac
  root=$(cd "$R" 2>/dev/null && pwd -P) || { echo gone; return 0; }
  case "$root" in /|"$(cd "$HOME" 2>/dev/null && pwd -P)") refuse "the root is / or the home directory: $root" ;; esac
  [ -e "$R/$A1" ] || { echo gone; tidy "$root" "${A1%/*}"; return 0; }
  [ ! -L "$R/$A1" ] && [ -d "$R/$A1" ] || refuse "$A1 is not a plain directory"
  space=${A1%%/*}
  pool=${A1#*/}; pool=${pool%/*}
  [ "$(head -n1 "$R/$A1/marks/vivi_pool" 2>/dev/null)" = "$pool" ] || refuse "$A1 carries no Vivi mark for pool $pool"
  [ "$(head -n1 "$R/$A1/marks/vivi_pool_mailspace" 2>/dev/null)" = "$space" ] || refuse "$A1 carries no Vivi mark for mailspace $space"
  real=$(cd "$R/$A1" && pwd -P)
  case "$real" in "$root"/*/*/slot-*) ;; *) refuse "$real is outside $root" ;; esac
  stop_slot
  rm -rf -- "$real" || fail "could not delete $A1"
  tidy "$root" "${A1%/*}"
  echo destroyed
}

# Remove the pool directory, the mailspace directory and, if Vivi made it, the
# root, as each becomes empty. $2 is <mailspace>/<pool>.
tidy() {
  rmdir "$1/$2" 2>/dev/null
  rmdir "$1/${2%/*}" 2>/dev/null
  if [ -f "$1/.vivi-root" ] && [ "$(ls -A "$1")" = ".vivi-root" ]; then
    rm -f "$1/.vivi-root" && rmdir "$1"
  fi
  return 0
}

probe() {
  local t anchor
  echo "user=$(id -un)"
  echo "host=$(hostname 2>/dev/null)"
  echo "system=$(uname -s) $(uname -m)"
  for t in git tar timeout sha256sum ps od awk sed; do
    command -v "$t" > /dev/null 2>&1 || echo "missing=$t"
  done
  if [ "$SYSTEM" = Darwin ]; then
    command -v setsid >/dev/null 2>&1 || { command -v perl >/dev/null 2>&1 || echo "missing=perl (setsid fallback)"; }
  else
    command -v setsid >/dev/null 2>&1 || echo "missing=setsid"
    [ -r /proc/self/environ ] || echo "missing=/proc"
  fi
  echo | grep -z '' > /dev/null 2>&1 || echo "missing=grep -z"
  anchor=$R
  while [ ! -e "$anchor" ] && [ "$anchor" != / ]; do anchor=$(dirname "$anchor"); done
  echo "root=$R"
  [ -d "$R" ] && echo "root_exists=1" || echo "root_exists=0"
  [ -d "$anchor" ] && [ -w "$anchor" ] && echo "root_writable=1" || echo "root_writable=0"
  echo "root_anchor=$anchor"
  df -Pk "$anchor" 2>/dev/null | awk 'NR == 2 { print "free_kb=" $4 }'
}

envgaps() {
  local n
  for n in $A1; do
    [ -n "${!n:-}" ] || echo "$n"
  done
  return 0
}

exec_script() {
  need_slot "$A1"; cd "$D" || fail "no such slot: $A1"
  export WORK="$D"
  bash -c "$A2"
}

busy_lock
case "$VERB" in
  list) list_slots ;;
  machine) need_slot "$A1"; if known || [ -n "${EXPLICIT_DIR:-}" ]; then emit "$A1"; fi ;;
  probe) probe ;;
  envgaps) envgaps ;;
  create) create_slot ;;
  install)
    need_slot "$A1"
    [ "$(sed -n 's/^nonce=//p' "$D/claim")" = "$A2" ] || fail "not claim holder"
    tar -xf - -C "$D" || fail "cannot install runtime"
    chmod +x "$D"/bin/*
    if [ -e "$EXPLICIT_DIR/.git" ]; then
      (
        cd "$EXPLICIT_DIR" || exit 1
        exclude=$(git rev-parse --git-path info/exclude) || exit 1
        mkdir -p "$(dirname "$exclude")" || exit 1
        grep -qxF '/.vivi/' "$exclude" 2>/dev/null || printf '\n/.vivi/\n' >> "$exclude"
      ) || fail "cannot exclude slot records"
    fi
    touch "$D/seeded"
    printf '%s\n' ready > "$D/marks/vivi_pool_state"
    ;;
  start) start_slot ;;
  stop) stop_slot ;;
  claim) claim_slot ;;
  unclaim) need_slot "$A1"; busy_release; rm -f "$D/claim" ;;
  put) put_file ;;
  seal) seal_mark ;;
  read) read_files ;;
  exec) exec_script ;;
  destroy) destroy_slot ;;
  *) fail "unknown verb: $VERB" ;;
esac
