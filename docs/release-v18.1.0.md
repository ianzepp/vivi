# Vivi 18.1.0

Minor release on 18.0.0. Additions for vivi-server to serve a host's own
mailspace as a project (want c031380c): one host project registry, a public
filing path, a store change watermark, the board as a library model, and a
guard against older code opening a newer store.

## Host project registry (`vivi::registry`)

`<config_dir>/vivi/pump-service.toml` (`VIVI_CONFIG_DIR` overrides
`<config_dir>`) is now the host's project registry, read by the pump
supervisor and by servers. Each project carries two flags:

```toml
[[project]]
root = "/mnt/vivi/projects/faberlang"
pump = true    # the pump supervisor runs a pump for it (default true)
serve = false  # a server hosts it (default false)
```

- The earlier `projects = ["/path", ...]` list still reads, as pump on and
  serve off, and is rewritten in the table format on the next write.
- `vivi pump service add <root>` takes `--pump`/`--no-pump` and
  `--serve`/`--no-serve`; on an existing entry only the flags passed change.
  `vivi pump service list` prints each root with its flags.
- The supervisor runs pumps only for `pump = true`.

**Upgrade order:** 18.0.0 cannot read the table format. Upgrade every vivi
that reads the registry on a host (the supervisor and the CLI) before the
first write with 18.1.0.

## Other additions

- **`vivi::sync::pull::file_records`.** It files already-verified records
  into a mailspace exactly as a pull files a page: verify, file by address
  into role folders, bind, link replies, fold item events. `pull` now uses
  the same path.
- **`Storage::change_watermark()`.** A cheap "did this store change"
  fingerprint: the highest message rowid, mailspace event id and work-graph
  event id. It does not cover in-place edits that log no event.
- **`vivi::mailspace::board`.** `Mailspace::board(&BoardOptions)` returns the
  `Serialize` board model that `vivi board` prints. The CLI output is
  unchanged.

## Changed

- **A newer store is refused.** Opening a mailspace whose store schema is
  newer than this vivi's now fails, naming both versions, instead of
  silently rewriting the version marker.
