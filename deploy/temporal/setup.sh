#!/usr/bin/env bash
# setup.sh: install Postgres and the Temporal server on hub under systemd.
#
#   sudo ./setup.sh            (run from the staged directory; see README.md)
#
# Run fetch.sh <dir>/bundle first (no root). Safe to run again: every step
# checks before it changes anything. Prints no secrets.
#
# What it does:
#   1. pacman: postgresql
#   2. Postgres data on /mnt/data/postgres (drop-in), initdb once, enable+start
#   3. role `temporal` (generated password in /etc/temporal/db-password, 0600)
#      and databases temporal, temporal_visibility
#   4. system user `temporal`, binaries into /usr/local/bin
#   5. schema setup/update from the bundle's postgresql/v12 schema
#   6. /etc/temporal/temporal.yaml (0640 root:temporal), UI config, units;
#      enable+start temporal-server and temporal-ui
#   7. namespace `default` (30-day retention)
#   8. ufw: 7233 (frontend) and 8233 (UI) from 192.168.1.0/24 on enp0s31f6
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
bundle=$here/bundle
PGROOT=/mnt/data/postgres
LAN=192.168.1.0/24
IFACE=enp0s31f6

[ "$(id -u)" = 0 ] || { echo "run with sudo" >&2; exit 1; }
[ -x "$bundle/bin/temporal-server" ] || { echo "no bundle: run fetch.sh $bundle first" >&2; exit 1; }
step() { printf '\n== %s\n' "$*"; }

step "1. postgresql package"
pacman -S --needed --noconfirm postgresql
grep -q 'PGROOT' /usr/lib/systemd/system/postgresql.service \
  || { echo "postgresql.service does not use PGROOT; the data drop-in would not apply" >&2; exit 1; }

step "2. Postgres data on $PGROOT"
install -d -m 0755 /etc/systemd/system/postgresql.service.d
install -m 0644 "$here/postgresql-data.conf" /etc/systemd/system/postgresql.service.d/data.conf
install -d -o postgres -g postgres -m 0700 "$PGROOT"
if [ ! -f "$PGROOT/data/PG_VERSION" ]; then
  runuser -u postgres -- initdb -D "$PGROOT/data" --locale=C.UTF-8 --encoding=UTF8 \
    --auth-local=peer --auth-host=scram-sha-256
fi
systemctl daemon-reload
systemctl enable --now postgresql
for _ in $(seq 30); do runuser -u postgres -- pg_isready -q && break; sleep 1; done

step "3. role and databases"
install -d -m 0750 -o root -g root /etc/temporal
if [ ! -s /etc/temporal/db-password ]; then
  umask 077; head -c 32 /dev/urandom | base64 | tr -d '/+=\n' | head -c 40 > /etc/temporal/db-password
fi
chmod 0600 /etc/temporal/db-password
runuser -u postgres -- psql -q -v ON_ERROR_STOP=1 -v pw="$(cat /etc/temporal/db-password)" <<'SQL'
SELECT 'CREATE ROLE temporal LOGIN' WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'temporal')\gexec
ALTER ROLE temporal WITH LOGIN PASSWORD :'pw';
SELECT 'CREATE DATABASE temporal OWNER temporal' WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'temporal')\gexec
SELECT 'CREATE DATABASE temporal_visibility OWNER temporal' WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'temporal_visibility')\gexec
SQL

step "4. user and binaries"
id -u temporal >/dev/null 2>&1 || useradd --system --home-dir /var/lib/temporal --shell /usr/bin/nologin temporal
install -d -o temporal -g temporal -m 0750 /var/lib/temporal
chgrp temporal /etc/temporal
for b in temporal-server temporal-sql-tool temporal-ui-server temporal; do
  install -m 0755 "$bundle/bin/$b" "/usr/local/bin/$b"
done

step "5. schema"
sqltool() { temporal-sql-tool --plugin postgres12 --ep 127.0.0.1 -p 5432 -u temporal \
  --pw "$(cat /etc/temporal/db-password)" --db "$1" "${@:2}"; }
has_schema() { runuser -u postgres -- psql -tAq -d "$1" \
  -c "SELECT 1 FROM information_schema.tables WHERE table_name = 'schema_version'" | grep -q 1; }
for pair in temporal:temporal temporal_visibility:visibility; do
  db=${pair%%:*} dir=${pair##*:}
  has_schema "$db" || sqltool "$db" setup-schema -v 0.0
  sqltool "$db" update-schema -d "$bundle/schema/postgresql/v12/$dir/versioned"
done

step "6. config and units"
umask 027
sed "s|@@DB_PASSWORD@@|$(cat /etc/temporal/db-password)|g" "$here/temporal.yaml.in" > /etc/temporal/temporal.yaml.new
chown root:temporal /etc/temporal/temporal.yaml.new && chmod 0640 /etc/temporal/temporal.yaml.new
mv /etc/temporal/temporal.yaml.new /etc/temporal/temporal.yaml
install -d -o root -g temporal -m 0750 /etc/temporal/ui /etc/temporal/ui/config
for env in base production; do
  install -o root -g temporal -m 0640 "$here/ui.yaml" "/etc/temporal/ui/config/$env.yaml"
done
install -m 0644 "$here/temporal-server.service" /etc/systemd/system/temporal-server.service
install -m 0644 "$here/temporal-ui.service" /etc/systemd/system/temporal-ui.service
systemctl daemon-reload
systemctl enable temporal-server temporal-ui
systemctl restart temporal-server
for _ in $(seq 60); do
  temporal operator cluster health --address 127.0.0.1:7233 >/dev/null 2>&1 && break; sleep 1
done
temporal operator cluster health --address 127.0.0.1:7233
systemctl restart temporal-ui

step "7. namespace default"
temporal operator namespace describe --address 127.0.0.1:7233 -n default >/dev/null 2>&1 \
  || temporal operator namespace create --address 127.0.0.1:7233 -n default --retention 30d

step "8. firewall"
ufw allow in on "$IFACE" from "$LAN" to any port 7233 proto tcp comment temporal-frontend-lan
ufw allow in on "$IFACE" from "$LAN" to any port 8233 proto tcp comment temporal-ui-lan

step "done"
systemctl --no-pager --lines=0 status postgresql temporal-server temporal-ui | grep -E '●|Active:'
echo "Frontend: hub.local:7233   UI: http://hub.local:8233   Postgres data: $PGROOT/data"
