source /private/tmp/claude-501/-Users-ianzepp-work-ianzepp-vivi/2214cfc8-c994-47ee-8080-9f3236d10e73/scratchpad/audit-work/env.sh
v() { "$VIVI" "$@"; echo "  [exit=$?]"; }
sa() { "$VIVI" sync --project "$A" 2>&1 | sed 's/^/  A: /'; }
sb() { "$VIVI" sync --project "$B" 2>&1 | sed 's/^/  B: /'; }
sync2() { sa; sb; sa; }
