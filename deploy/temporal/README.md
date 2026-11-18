# Temporal on hub

The self-hosted Temporal server for the orchestration reset ([`docs/orchestration-reset/`](../../docs/orchestration-reset/README.md)). It runs as one process under systemd, with Postgres on hub's 2 TB data disk.

| Piece | Where |
|---|---|
| Postgres (pacman `postgresql`) | `postgresql.service`. Data in `/mnt/data/postgres/data`, set by the drop-in `postgresql-data.conf` |
| Temporal server 1.32.2 | `temporal-server.service`, user `temporal`. Config `/etc/temporal/temporal.yaml` (0640 root:temporal) |
| Temporal UI 2.58.1 | `temporal-ui.service` on port 8233. Config `/etc/temporal/ui/config/` |
| CLI 1.10.1 | `/usr/local/bin/temporal` |
| Database password | `/etc/temporal/db-password` (0600 root), generated once |
| Network | Frontend `hub.local:7233` and UI `http://hub.local:8233`. ufw admits both from `192.168.1.0/24` only. Every other port listens on loopback or is closed by ufw |

There is no authorizer (`--allow-no-auth`): the LAN is the access boundary, as for the model proxy and vivi-server.

## Install

```sh
# 1. No root: download the pinned, checksum-verified binaries and the Postgres schema.
./fetch.sh "$PWD/bundle"
# 2. Root: install and start everything. Safe to run again.
sudo ./setup.sh
```

The staged copy on hub is `~/temporal-setup/`. Its bundle was fetched on 2026-11-18.

## Checked before handover (2026-11-18)

On hub as an ordinary user, the same server config was run with in-memory SQLite in place of Postgres:
- `temporal operator cluster health` answered `SERVING`;
- the `default` namespace was created and described over the LAN address;
- the UI answered 200 on loopback and on the LAN address;
- only the frontend (7233, 6933, 7243) bound beyond loopback.

The Postgres path (`setup.sh` steps 1–5) has not run yet, because it needs root.

## Upgrade

Raise the versions and the source checksum in `fetch.sh`, then fetch again and rerun `setup.sh`. Step 5 applies any new versioned schema before the server restarts.
