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
  platform="$(uname -s)-$(uname -m)"
  case "$platform" in
    Linux-x86_64|Linux-amd64) echo "linux_amd64" ;;
    Linux-aarch64|Linux-arm64) echo "linux_arm64" ;;
    Darwin-arm64|Darwin-aarch64) echo "darwin_arm64" ;;
    Darwin-x86_64|Darwin-amd64)
      echo "Unsupported platform: macOS Intel is not supported." >&2
      exit 1
      ;;
    Linux-*)
      echo "Unsupported Linux architecture: ${platform#Linux-}" >&2
      exit 1
      ;;
    Darwin-*)
      echo "Unsupported macOS architecture: ${platform#Darwin-}" >&2
      exit 1
      ;;
    *)
      echo "Unsupported platform: $platform" >&2
      exit 1
      ;;
  esac
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
