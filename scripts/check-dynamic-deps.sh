#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <binary>" >&2
  exit 2
fi

bin="$1"

if [[ ! -f "$bin" ]]; then
  echo "binary not found: $bin" >&2
  exit 2
fi

case "$(uname -s)" in
  Darwin)
    unexpected=0
    while IFS= read -r dep; do
      [[ -z "$dep" ]] && continue
      case "$dep" in
        /System/Library/*|/usr/lib/*)
          ;;
        *)
          echo "unexpected dynamic dependency: $dep" >&2
          unexpected=1
          ;;
      esac
    done < <(otool -L "$bin" | tail -n +2 | awk '{print $1}')
    exit "$unexpected"
    ;;
  Linux)
    unexpected=0
    while IFS= read -r dep; do
      [[ -z "$dep" ]] && continue
      case "$dep" in
        libc.so.*|libgcc_s.so.*|libm.so.*|libpthread.so.*|librt.so.*|libdl.so.*|libutil.so.*|libresolv.so.*|libanl.so.*|libnsl.so.*)
          ;;
        *)
          echo "unexpected dynamic dependency: $dep" >&2
          unexpected=1
          ;;
      esac
    done < <(readelf -d "$bin" | sed -n 's/.*Shared library: \[\(.*\)\]/\1/p')
    exit "$unexpected"
    ;;
  *)
    echo "unsupported host OS: $(uname -s)" >&2
    exit 2
    ;;
esac
