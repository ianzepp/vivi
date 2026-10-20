#!/bin/sh
# Vivi installer.
#
# Installs one released binary as <prefix>/bin/vivi-<version> and points
# <prefix>/bin/vivi at it. Versions sit side by side; `vivi update` moves the
# symlink, and `vivi update --version <VERSION>` switches back.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/ianzepp/vivi/main/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --version 16.0.2 --prefix "$HOME/.local"
#
# The default version is the release this script was exported from. Pass
# --version to install another published release. A plain-file
# <prefix>/bin/vivi (not a symlink) is refused unless --adopt / --force
# is passed; adopting moves it to vivi.unmanaged instead of deleting it.

set -eu

PREFIX="${VIVI_PREFIX:-${HOME:?HOME is not set}/.local}"
VERSION="${VIVI_VERSION:-16.0.2}"
BASE_URL="${VIVI_BASE_URL:-https://github.com/ianzepp/vivi/releases/download}"
ADOPT=0

usage() {
    cat <<'EOF'
usage: install.sh [--version VERSION] [--prefix DIR] [--base-url URL] [--adopt]

  --version VERSION  release to install (default: the release this script came from)
  --prefix DIR       install prefix (default: ~/.local; binaries in <DIR>/bin)
  --base-url URL     release asset base (default: GitHub releases)
  --adopt, --force   move a plain-file vivi aside to vivi.unmanaged (never delete)

Installs <prefix>/bin/vivi-VERSION and points <prefix>/bin/vivi at it.
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="${2:?--version needs a value}"; shift 2 ;;
        --version=*) VERSION="${1#--version=}"; shift ;;
        --prefix) PREFIX="${2:?--prefix needs a value}"; shift 2 ;;
        --prefix=*) PREFIX="${1#--prefix=}"; shift ;;
        --base-url) BASE_URL="${2:?--base-url needs a value}"; shift 2 ;;
        --base-url=*) BASE_URL="${1#--base-url=}"; shift ;;
        --adopt|--force) ADOPT=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

VERSION="${VERSION#v}"
BASE_URL="${BASE_URL%/}"

mailspace_root() {
    dir="$1"
    while [ -n "$dir" ] && [ "$dir" != "/" ]; do
        if [ -f "$dir/mailspace.toml" ]; then
            printf '%s\n' "$dir"
            return 0
        fi
        next="$(dirname "$dir")"
        if [ "$next" = "$dir" ]; then
            break
        fi
        dir="$next"
    done
    return 1
}

# A plain-file <bin>/vivi is refused unless --adopt. Adopting only moves
# that one file to vivi.unmanaged (or .N). Shared-bin neighbors stay.
inspect_vivi_link() {
    link="$1"
    if [ ! -e "$link" ] && [ ! -L "$link" ]; then
        return 0
    fi
    if [ -L "$link" ]; then
        return 0
    fi
    if [ ! -f "$link" ]; then
        echo "error: ${link} exists and is not a vivi file or symlink" >&2
        echo "  next: ~/.local/bin is shared; remove or rename that path yourself" >&2
        exit 1
    fi
    if [ "$ADOPT" -ne 1 ]; then
        echo "error: ${link} is a plain file, not a vivi symlink" >&2
        echo "  next: pass --adopt (or --force) to move it aside to vivi.unmanaged" >&2
        exit 1
    fi
}

adopt_vivi_link() {
    link="$1"
    inspect_vivi_link "$link"
    if [ ! -f "$link" ] || [ -L "$link" ]; then
        return 0
    fi
    dest="$(dirname "$link")/vivi.unmanaged"
    n=1
    while [ -e "$dest" ] || [ -L "$dest" ]; do
        dest="$(dirname "$link")/vivi.unmanaged.${n}"
        n=$((n + 1))
    done
    mv "$link" "$dest"
    echo "moved ${link} aside to ${dest}"
}

if root="$(mailspace_root "${PREFIX}/bin")"; then
    echo "error: ${PREFIX} sits inside a Vivi mailspace (${root}/mailspace.toml)" >&2
    echo "  next: use the default prefix ~/.local, not a project .vivi directory" >&2
    exit 1
fi
inspect_vivi_link "${PREFIX}/bin/vivi"

case "$VERSION" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) echo "error: --version must look like 14.1.0 (got ${VERSION})" >&2; exit 2 ;;
esac

case "$(uname -s):$(uname -m)" in
    Darwin:arm64) TRIPLE="aarch64-apple-darwin" ;;
    Darwin:x86_64) TRIPLE="x86_64-apple-darwin" ;;
    Linux:x86_64) TRIPLE="x86_64-unknown-linux-gnu" ;;
    *)
        echo "error: no published vivi binary for $(uname -s) $(uname -m); releases support aarch64-apple-darwin, x86_64-apple-darwin, and x86_64-unknown-linux-gnu" >&2
        exit 1
        ;;
esac

command -v curl >/dev/null 2>&1 || { echo "error: curl is required" >&2; exit 1; }
command -v tar >/dev/null 2>&1 || { echo "error: tar is required" >&2; exit 1; }

archive="vivi-${TRIPLE}.tar.gz"
url="${BASE_URL}/v${VERSION}/${archive}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

echo "vivi install — ${VERSION} (${TRIPLE})"
echo "  prefix:   ${PREFIX}/bin"
echo "  download: ${url}"

download() {
    dest="$1"
    src="$2"
    if [ -f "$src" ]; then
        cp "$src" "$dest"
        return 0
    fi
    token="${GITHUB_TOKEN:-${GH_TOKEN:-}}"
    if [ -n "$token" ]; then
        curl -fsSL -H "Authorization: Bearer ${token}" "$src" -o "$dest"
    else
        curl -fsSL "$src" -o "$dest"
    fi
}

download "${tmp}/${archive}" "$url"
download "${tmp}/${archive}.sha256" "${url}.sha256"

if command -v shasum >/dev/null 2>&1; then
    (cd "$tmp" && shasum -a 256 -c "${archive}.sha256")
elif command -v sha256sum >/dev/null 2>&1; then
    (cd "$tmp" && sha256sum -c "${archive}.sha256")
else
    echo "error: shasum or sha256sum is required to verify the download" >&2
    exit 1
fi

mkdir -p "${PREFIX}/bin"
tar -xzf "${tmp}/${archive}" -C "$tmp"
if [ -f "${tmp}/vivi" ]; then
    src="${tmp}/vivi"
elif [ -f "${tmp}/bin/vivi" ]; then
    src="${tmp}/bin/vivi"
else
    echo "error: the release archive does not contain vivi" >&2
    exit 1
fi
mv "$src" "${PREFIX}/bin/vivi-${VERSION}"
chmod 755 "${PREFIX}/bin/vivi-${VERSION}"

adopt_vivi_link "${PREFIX}/bin/vivi"
rm -f "${PREFIX}/bin/.vivi.link.tmp"
ln -s "vivi-${VERSION}" "${PREFIX}/bin/.vivi.link.tmp"
mv -f "${PREFIX}/bin/.vivi.link.tmp" "${PREFIX}/bin/vivi"

echo "installed ${PREFIX}/bin/vivi-${VERSION}"
echo "active:    ${PREFIX}/bin/vivi -> vivi-${VERSION}"
case ":${PATH}:" in
    *":${PREFIX}/bin:"*) ;;
    *) echo "add to PATH: export PATH=\"${PREFIX}/bin:\$PATH\"" ;;
esac
on_path="$(command -v vivi 2>/dev/null || true)"
managed="${PREFIX}/bin/vivi"
if [ -n "$on_path" ] && [ "$on_path" != "$managed" ]; then
    echo "warning: a different vivi is earlier on PATH ($on_path) than the managed install ($managed)" >&2
fi
"${PREFIX}/bin/vivi" --version
echo "note: if this release changed skills, refresh installed copies: vivi skills install <dir> --force"
for unit in "${HOME}/Library/LaunchAgents/"com.vivi*.plist; do
    [ -f "$unit" ] || continue
    echo "note: restart ${unit} to pick up the new binary: launchctl bootout gui/$(id -u) ${unit} && launchctl bootstrap gui/$(id -u) ${unit}"
done
for unit in "${HOME}/.config/systemd/user/"vivi*.service; do
    [ -f "$unit" ] || continue
    echo "note: restart ${unit##*/} to pick up the new binary: systemctl --user restart ${unit##*/}"
done
