# Vivi storage write audit: fix in SQLite, or move to Postgres?

**Status**: measured 2026-11-18 on hub (vivi 24.0.0, ext4 on sda). A recommendation for the operator.

## Answer

**Stay on SQLite.** The whole-database rewrite is a code pattern in the **pump's** store (`pump.db`). It is not how Vivi's record (`mail.sqlite`) writes, and it is not something SQLite forces.

- Vivi's record writes row by row. The cost of one operation stays nearly flat as the store grows.
- Postgres would not write fewer bytes for the same work. It writes its own WAL, the table and every index, plus full-page images after each checkpoint.

The better reasons to move to Postgres are elsewhere; see below.

## What was measured

[`vivi_write_audit.py`](vivi_write_audit.py) builds a mailspace on a real disk and grows it with `vivi need send` (1 KB bodies). At each size it measures one operation:

- **Bytes written:** the kernel's `write_bytes` for the child process. These are bytes sent to the block layer, so writes to `/dev/null` and pipes are excluded.
- **Pages and transactions:** [`walparse.py`](walparse.py) reads the SQLite WAL while a second connection holds it open, which stops SQLite deleting it.

### Bytes written per operation, by store size

| Needs | DB size | send | mark | done | board (read) |
|---|---|---|---|---|---|
| 20 | 0.3 MB | 816 KB | 568 KB | 776 KB | 32 KB, 0.01 s |
| 300 | 1.2 MB | 900 KB | 656 KB | 912 KB | 32 KB, 0.15 s |
| 1,500 | 4.8 MB | 932 KB | 608 KB | 872 KB | 32 KB, **2.6 s** |
| 3,000 | 9.3 MB | 1,092 KB | 760 KB | 984 KB | 128 KB, **10.5 s** |

The database grew about 30×, and the bytes per operation grew about 1.3×. That is b-tree depth, not a rewrite.

### Where the bytes go

| Op | WAL frames (4 KB pages) | Commits | Distinct pages | Tables and indexes touched |
|---|---|---|---|---|
| mark | 25 | 2 | 25 | about 20: `messages` and 6 of its indexes, `message_metadata` and 3 indexes, `mailspace_events` and 2, `mailspace_links` and 2, `blobs` and 2, `sqlite_sequence`, and page 1 |
| send | 54 | 5 | 43 | the same set, plus `work_graphs` and `work_graph_nodes` and their indexes |

- **Logical cost:** a mark is a new letter row, and about 20 b-trees each take one changed leaf page, so 25 pages, about 100 KB. A send is 43 pages, but it commits 5 times, so some pages are written more than once.
- **Physical cost** is 4–6× the logical cost, about 0.6–1.1 MB, because each `vivi` CLI process opens and closes the database:
  1. the WAL is created and its frames written;
  2. the last connection to close checkpoints those pages into `mail.sqlite`, writing them a second time;
  3. the WAL is deleted;
  4. the shared-memory index (32 KB) is rebuilt;
  5. ext4 adds its journal and fsync overhead.
- **The vivi server avoids most of this.** It keeps a long-lived connection per project, so the WAL persists and checkpoints batch repeated pages. I inferred this from `Project::open` in `crates/vivi-server/src/server/projects.rs`; it was not measured.

### What that means for the drive

- **Projection:** at the peak pump load seen (about 950 units/day) with, say, 20 record writes per unit, the CLI path costs about 20 GB/day at 1 MB per operation. The server path costs less.
- **Drive life:** on the 500 GB 850 EVO (rated about 150 TBW, from memory), that is decades. The pump's whole-state rewrite was 1.8 TB/day, about 90× more, and it is already stopped.
- The record's write volume is not a reason to change databases.

## The real problem the audit found: board reads scale badly

`vivi board` took 0.15 s at 300 needs, 2.6 s at 1,500 and 10.5 s at 3,000. That is roughly quadratic.

- Faberlang has 4,405 needs, so a board read there likely takes 20 s or more *(extrapolated)*.
- This fits the hub reports: idle pump ticks of 1.7 s, MCP reads of about 21 s, and `absorb` at 1.4 s per call.
- It is CPU and read cost, not writes. It is a query or algorithm defect, most likely per-item lookups inside a loop. That is fixable in SQLite.
- It matters for the new design, because Temporal intake reads open needs from Vivi.

## Recommendations

1. **Pump store (`pump.db`).** It is retired with the pump in Phase 4. If any pump must run before then, change `PumpStore::save` to write only changed rows, and trim the `jobs` blob.
2. **Board read cost: fix before Phase 2 intake.** Profile `vivi board` at 3,000 needs, find the per-item query, and add a test that keeps board time roughly linear.
3. **Write-cost guard.** Add a test that grows a store to N letters and asserts that the WAL frames for one `need send` and one `need mark` stay under a bound that does not depend on N. This keeps a whole-store rewrite from slipping into the record unnoticed.
4. **Optional trims, only if the write rate ever matters:**
   - one transaction per CLI command (a send commits 5 times);
   - drop indexes no query uses (a letter touches about 20 b-trees);
   - writes routed through the server's long-lived connection instead of short CLI opens.

## When Postgres is the right call

These are worth weighing, and none of them is write volume:

- **Several machines writing one record at once.** SQLite allows one writer per file, and it should not live on a network filesystem.
- **Queries across projects** (the console, reports) without opening each mailspace in turn.
- **Running one database server.** Temporal will run on Postgres on hub anyway, so a Vivi schema there later would cost little.

Moving Vivi's record to Postgres is a reasonable later step for those reasons. It is not needed to fix wear, and it would not have prevented the 1.8 TB/day incident: that was the pump's save pattern, and the same pattern on Postgres would write just as much.
