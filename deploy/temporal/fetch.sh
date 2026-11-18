#!/usr/bin/env bash
# fetch.sh <bundle-dir>: download the pinned Temporal release binaries (checksum
# verified) and the Postgres schema from the matching source tag into a bundle
# directory that setup.sh installs from. Needs no root. Linux x86_64 only.
set -euo pipefail

SERVER_VERSION=1.32.2
UI_VERSION=2.58.1
CLI_VERSION=1.10.1
# sha256 of the GitHub source archive for v${SERVER_VERSION}; it carries the
# schema files the release tarball leaves out. Recorded 2026-11-18.
SOURCE_SHA256=f56f8c4e2ea10e50cfddc8d66bbba54fcff4f157ae3dfcefc179acfc5de0315c

out=${1:?usage: fetch.sh <bundle-dir>}
mkdir -p "$out/bin" "$out/dl"
cd "$out/dl"

get() { curl -fsSL --retry 3 -o "$2" "$1"; }
check() { # check <checksums file> <asset>
  grep " $2\$" "$1" | sha256sum -c - >/dev/null || { echo "checksum mismatch: $2" >&2; exit 1; }
}

gh=https://github.com
srv=temporal_${SERVER_VERSION}_linux_amd64.tar.gz
get "$gh/temporalio/temporal/releases/download/v$SERVER_VERSION/$srv" "$srv"
get "$gh/temporalio/temporal/releases/download/v$SERVER_VERSION/checksums.txt" server.sha256
check server.sha256 "$srv"
tar xzf "$srv" -C "$out/bin" temporal-server temporal-sql-tool

ui=ui-server_${UI_VERSION}_linux_amd64.tar.gz
get "$gh/temporalio/ui-server/releases/download/v$UI_VERSION/$ui" "$ui"
get "$gh/temporalio/ui-server/releases/download/v$UI_VERSION/ui-server_${UI_VERSION}_checksums.txt" ui.sha256
check ui.sha256 "$ui"
tar xzf "$ui" ui-server && mv ui-server "$out/bin/temporal-ui-server"

cli=temporal_cli_${CLI_VERSION}_linux_amd64.tar.gz
get "$gh/temporalio/cli/releases/download/v$CLI_VERSION/$cli" "$cli"
get "$gh/temporalio/cli/releases/download/v$CLI_VERSION/checksums.txt" cli.sha256
check cli.sha256 "$cli"
tar xzf "$cli" -C "$out/bin" temporal

src=temporal-src-v$SERVER_VERSION.tar.gz
get "$gh/temporalio/temporal/archive/refs/tags/v$SERVER_VERSION.tar.gz" "$src"
echo "$SOURCE_SHA256  $src" | sha256sum -c - >/dev/null || { echo "checksum mismatch: $src" >&2; exit 1; }
rm -rf "$out/schema"
tar xzf "$src" -C "$out" --strip-components=1 "temporal-$SERVER_VERSION/schema/postgresql/v12"
test -d "$out/schema/postgresql/v12/temporal/versioned"

chmod 755 "$out"/bin/*
echo "bundle ready: $out (server $SERVER_VERSION, ui $UI_VERSION, cli $CLI_VERSION)"
