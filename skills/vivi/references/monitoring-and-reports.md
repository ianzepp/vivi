# Monitoring and reports

## Live lane monitor

`vivi monitor` reads the pump-service registry and combines those project roots
with any roots passed to `--project`. Repeat `--project` for several extra
roots; duplicate canonical paths are removed. It does not claim, stop, or
change work. Snapshot rows use persisted pump and pool metadata; they do not
inspect process arguments, environment values, or credentials. Enter opens the
selected slot's raw log tail, whose contents depend on the worker output and
are not redacted by the monitor.

```sh
vivi monitor [--project <root> ...] [--once [--json]]
```

Without `--once`, the terminal view refreshes every second. It shows pump and
slot rows, with finished rows dimmed. Use Up/Down or `j`/`k` to select a row,
Enter to open its live log tail, then Esc or `q` to return; Esc or `q` in the
list quits. PageUp/PageDown scrolls the list or tail, and Home or `g` follows
the newest tail output.

`--once` prints one table snapshot and exits. Add `--json` to print a JSON
array of the same rows; warnings are written to stderr, and `--json` requires
`--once`. JSON rows include these fields; the terminal table shows the live
lane columns:

| Field | Meaning |
| --- | --- |
| `project`, `kind` | Mailspace name; `pump` or `slot` |
| `pool`, `slot`, `provider` | Pool and slot identity, plus where the lane runs |
| `role`, `task`, `subject`, `model` | Work and execution labels; `task` is a task handle for slots or a stable cycle identity for pump rows |
| `need`, `work_kind` | Parent need and work kind when the task or pool record provides them |
| `state` | `"running"` or `{ "finished": "<outcome>" }` |
| `started_at`, `running_secs`, `budget_secs` | Start time and elapsed/budget seconds; nullable when unavailable |
| `read_only`, `remote` | Optional read-only flag and whether the lane is remote |
| `tail` | Optional log path for the row |

The snapshot is assembled from Vivi's records and pool slots. It can report a
warning for an unreadable project or state file while still returning rows
from other registered roots.

## Attempt and need reports

Reports read attempt records and lifecycle timestamps already in the selected
mailspace. They use read-only storage and do not run a pump or dispatch work.

```sh
vivi report records --format csv|jsonl [filters]
vivi report throughput [filters] [--format table|json|csv|md]
vivi report durations [filters] [--format table|json|csv|md]
vivi report needs [filters] [--format table|json|csv|md]
vivi report failures [filters] [--format table|json|csv|md]
```

All views accept `--since <age|date|RFC3339>`, `--until <age|date|RFC3339>`,
`--need <handle>`, `--actor <name>`, `--model <name>`, and
`--project <root>`. Summary views also accept `--by actor|role|model|pool|kind|
need|day|hour` and `--top <count>`. `--since` and `--until` are inclusive.
A date-only `--since` starts at 00:00 UTC; a date-only `--until` includes that
whole UTC day through 23:59:59 UTC. RFC3339 and relative bounds compare as
inclusive instants; relative ages use `Nh`, `Nd`, or `Nw`.

Time filters use the event time relevant to each view: records and durations
use dispatch `start`; throughput bounds dispatched work by `start`, delivered
work by `delivered_at`, merged work by `merged_at`, and failures by `end`;
needs use `done_at` for a closed need and `opened_at` for an open one. Failures
use the failure end time. When a time bound is present, rows without the
timestamp used by that view cannot be placed in the window and are excluded.
Without bounds, they remain in the report with null or `unavailable` values.
Day and hour grouping uses UTC.

`records` requires `--format csv` or `--format jsonl`; CSV includes a header,
and JSONL writes one record per line. It exports the fixed attempt-record shape:

```json
{
  "need": "<handle>",
  "task": "<handle>",
  "attempt": "<unit>-a1",
  "actor": "<role>",
  "role": "<kind>",
  "pool": "<pool>",
  "slot": "<slot>",
  "model": "<model>",
  "kind": "<kind>",
  "start": 0,
  "end": 0,
  "agent_seconds": 0,
  "outcome": "<outcome>",
  "ready_at": 0,
  "delivered_at": 0,
  "merged_at": 0
}
```

Optional historical fields are JSON `null` when absent. Text and CSV render
missing cells as `unavailable`; they are not treated as zero.

Each summary view emits JSON rows with fixed fields:

| View | JSON row fields |
| --- | --- |
| `throughput` | `group`, `dispatched`, `delivered`, `merged`, `failed`, `unavailable` |
| `durations` | `group`, `measure`, `samples`, `unavailable`, `median_seconds`, `p90_seconds`, `max_seconds` |
| `needs` | `group`, `needs`, `open_needs`, `lead_seconds`, `unavailable_lead_times`, `effort_seconds`, `unavailable_agent_seconds`, `attempts` |
| `failures` | `group`, `count`, `repeat_units` |

Throughput defaults to `--by day`, durations to `--by need`, and needs to one
row per need. Failures default to grouping by outcome. For grouped needs,
`lead_seconds` is the mean of completed needs' open-to-done times; open needs
are counted in `open_needs`. If any completed need lacks endpoint timestamps,
`lead_seconds` is null and the missing count appears in
`unavailable_lead_times`. `effort_seconds` is the sum of attempt agent time
when every attempt has that field; otherwise it is null and missing agent-time
values are counted in `unavailable_agent_seconds`. With no attempts, effort
is zero.

Duration measures are `wait_while_ready` (`start - ready_at`), `agent_time`
(`agent_seconds`), and `delivered_to_merged` (`merged_at - delivered_at`).
The median averages the two center samples for an even-sized set using integer
seconds; p90 uses the nearest-rank sample. `repeat_units` lists unit ids with
more than one failed attempt. Output `unavailable` counts missing source
timestamps; null summary values stay null in JSON.
