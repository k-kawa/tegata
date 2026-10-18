#!/usr/bin/env bash
# Signs a draft release made by the release workflow, and publishes it. Run on the machine that
# holds the signing key (it asks for the key's password).
#   tools/sign-release.sh v0.1.0
#
# 1. downloads the files of the draft release and checks them against SHA256SUMS
# 2. checks that each file was built by this repository's release workflow (GitHub artifact
#    attestations)
# 3. signs SHA256SUMS with minisign, checks the signature with the official key, uploads
#    SHA256SUMS.minisig, and publishes the release
set -euo pipefail
tag=${1:?usage: tools/sign-release.sh <tag>}
repo=${REPO:-k-kawa/tegata}
key=${MINISIGN_KEY:-$HOME/.minisign/tegata.key}
# The official key (also in crates/tegata/src/install.rs and SECURITY.md)
public_key=RWS+RgaPKYHijXCDQ/JCehzLyohjXERYxShqSb6lS71/MHaCS3agTxkB

dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
gh release download "$tag" -R "$repo" -D "$dir"
cd "$dir"
[ ! -e SHA256SUMS.minisig ] || { echo "$tag is already signed" >&2; exit 1; }
shasum -a 256 -c SHA256SUMS
for f in tegata*; do
  gh attestation verify "$f" -R "$repo" --signer-workflow "$repo/.github/workflows/release.yml" >/dev/null
  echo "$f: built by $repo/.github/workflows/release.yml"
done
minisign -S -s "$key" -m SHA256SUMS -t "tegata $tag"
minisign -V -P "$public_key" -m SHA256SUMS
gh release upload "$tag" SHA256SUMS.minisig -R "$repo"
gh release edit "$tag" --draft=false -R "$repo"
echo "published $tag"
