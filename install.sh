#!/usr/bin/env sh
set -eu

VERSION=""
YES=0
PREFIX="${HOME}/.local/bin"

while [ $# -gt 0 ]; do
  case "$1" in
    --version)
      VERSION="$2"
      shift
      ;;
    --yes|-y)
      YES=1
      ;;
    --prefix)
      PREFIX="$2"
      shift
      ;;
    *)
      ;;
  esac
  shift
done

detect_arch() {
  os="$(uname -s | tr '[:upper:]' '[:lower:]')"
  arch="$(uname -m)"
  case "$os" in
    linux) os_part="linux" ;;
    darwin) os_part="darwin" ;;
    *) echo "Unsupported OS: $os" >&2; exit 1 ;;
  esac
  case "$arch" in
    x86_64|amd64) arch_part="amd64" ;;
    aarch64|arm64) arch_part="arm64" ;;
    *) echo "Unsupported arch: $arch" >&2; exit 1 ;;
  esac
  echo "${os_part}_${arch_part}"
}

fetch_latest_version() {
  api="https://api.github.com/repos/pfnet-research/sshpod/releases/latest"
  web="https://github.com/pfnet-research/sshpod/releases/latest"
  auth_arg=""
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    auth_arg="-H" "Authorization: Bearer ${GITHUB_TOKEN}"
  fi

  try_api() {
    if resp=$(curl -fsSL $auth_arg "$api" 2>/dev/null); then
      printf "%s" "$resp" | grep -m1 '"tag_name"' | sed -E 's/.*"v?([^"]+)".*/\1/' || true
    fi
  }

  try_redirect() {
    curl -fsSLI -o /dev/null -w '%{url_effective}' "$web" 2>/dev/null \
      | sed -E 's#.*/tag/v?([^/]+)$#\1#'
  }

  version="$(try_api)"
  if [ -z "$version" ]; then
    version="$(try_redirect)"
  fi
  if [ -n "$version" ]; then
    echo "$version"
    return 0
  fi

  echo "Failed to determine latest version from GitHub releases." >&2
  exit 1
}

if [ -z "$VERSION" ]; then
  VERSION="$(fetch_latest_version)"
fi

ARCH_NAME="$(detect_arch)"
ASSET="sshpod_${VERSION}_${ARCH_NAME}.tar.gz"
URL="https://github.com/pfnet-research/sshpod/releases/download/v${VERSION}/${ASSET}"

TMPDIR="$(mktemp -d)"
cleanup() { rm -rf "$TMPDIR"; }
trap cleanup EXIT

echo "Downloading ${ASSET}..."
curl -fL "$URL" -o "$TMPDIR/$ASSET"

mkdir -p "$TMPDIR/bin"
tar xzf "$TMPDIR/$ASSET" -C "$TMPDIR/bin"
mkdir -p "$PREFIX"
install -m 0755 "$TMPDIR/bin/sshpod" "$PREFIX/sshpod"

echo "Installed to $PREFIX/sshpod"

if command -v sshpod >/dev/null 2>&1; then
  :
else
  echo "Note: $PREFIX may need to be added to PATH" >&2
fi

if [ "$YES" -eq 1 ]; then
  "$PREFIX/sshpod" configure
else
  printf "Run sshpod configure to update ~/.ssh/config now? [y/N]: "
  if read ans && [ "${ans:-N}" = "y" ] || [ "${ans:-N}" = "Y" ]; then
    "$PREFIX/sshpod" configure
  else
    echo "Skipping ssh config update."
  fi
fi
