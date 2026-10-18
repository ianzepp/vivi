#!/bin/sh
# Vivi installer.
#
# Installs one released binary as <prefix>/bin/vivi-<version> and points
# <prefix>/bin/vivi at it. Versions sit side by side; `vivi update` moves the
# symlink, and `vivi update --version <VERSION>` switches back.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/ianzepp/vivi/main/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --version 14.1.0 --prefix "$HOME/.vivi"
#
# The default version is the release this script was exported from. Pass
# --version to install another published release.

set -eu

PREFIX="${VIVI_PREFIX:-${HOME:?HOME is not set}/.vivi}"
VERSION="${VIVI_VERSION:-14.1.0}"
BASE_URL="${VIVI_BASE_URL:-https://github.com/ianzepp/vivi/releases/download}"

usage() {
    cat <<'EOF'
usage: install.sh [--version VERSION] [--prefix DIR] [--base-url URL]

  --version VERSION  release to install (default: the release this script came from)
  --prefix DIR       install prefix (default: ~/.vivi)
  --base-url URL     release asset base (default: GitHub releases)

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
        -h|--help) usage; exit 0 ;;
        *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

VERSION="${VERSION#v}"
BASE_URL="${BASE_URL%/}"
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

rm -f "${PREFIX}/bin/.vivi.link.tmp"
ln -s "vivi-${VERSION}" "${PREFIX}/bin/.vivi.link.tmp"
mv -f "${PREFIX}/bin/.vivi.link.tmp" "${PREFIX}/bin/vivi"

echo "installed ${PREFIX}/bin/vivi-${VERSION}"
echo "active:    ${PREFIX}/bin/vivi -> vivi-${VERSION}"
case ":${PATH}:" in
    *":${PREFIX}/bin:"*) ;;
    *) echo "add to PATH: export PATH=\"${PREFIX}/bin:\$PATH\"" ;;
esac
"${PREFIX}/bin/vivi" --version
