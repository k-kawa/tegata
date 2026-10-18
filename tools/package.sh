#!/usr/bin/env bash
# Copies the release binaries of one target into a directory, named as `tegata init` expects:
# <binary>_<version>_<os>_<arch>.
#   tools/package.sh <rust target> <version> <out dir>
# Run after `cargo build --release --locked --target <rust target> --workspace --bins`.
set -euo pipefail
target=$1 version=$2 out=$3
case "$target" in
  x86_64-unknown-linux-musl) platform=linux_x86_64 ;;
  aarch64-unknown-linux-musl) platform=linux_aarch64 ;;
  x86_64-apple-darwin) platform=darwin_x86_64 ;;
  aarch64-apple-darwin) platform=darwin_aarch64 ;;
  *) echo "unsupported target: $target" >&2; exit 2 ;;
esac
root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$out"
for bin in tegata tegata-provider-aws tegata-provider-cloudflare tegata-provider-gcp \
  tegata-provider-tailscale tegata-provider-terraform; do
  cp "$root/target/$target/release/$bin" "$out/${bin}_${version}_${platform}"
done
ls -l "$out"
