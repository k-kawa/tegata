#!/usr/bin/env bash
# Prints the release notes of a version: its section of CHANGELOG.md, how to check the files, and
# the link to the full changes. The release workflow uses it for the draft release.
#   tools/release-notes.sh 0.2.0
#
# Fails if CHANGELOG.md has no section for the version, or the section is empty, so that a
# release is never published without notes.
set -euo pipefail
version=${1:?usage: tools/release-notes.sh <version>}
version=${version#v}
root=$(cd "$(dirname "$0")/.." && pwd)
changelog=$root/CHANGELOG.md
repo=k-kawa/tegata
# The official key (also in SECURITY.md, crates/tegata/src/install.rs and tools/sign-release.sh)
public_key=$(grep -m1 -oE 'RW[A-Za-z0-9+/]{54}' "$root/SECURITY.md")

# The lines after "## [<version>]" up to the next "## [" or the link definitions
section=$(awk -v v="$version" '
  index($0, "## [" v "]") == 1 { on = 1; next }
  on && (/^## \[/ || /^\[[^]]+\]: /) { exit }
  on { print }
' "$changelog")
if [ -z "$(printf '%s' "$section" | tr -d '[:space:]')" ]; then
  echo "error: CHANGELOG.md has no notes for $version (add a section \"## [$version] - <date>\")" >&2
  exit 1
fi
compare=$(grep -m1 -E "^\[$version\]: " "$changelog" | sed -E 's/^\[[^]]+\]: //' || true)

# The section, without the blank lines around it
printf '%s\n' "$section" | sed -e '/./,$!d' | awk '{ lines[NR] = $0 } END { n = NR; while (n > 0 && lines[n] == "") n--; for (i = 1; i <= n; i++) print lines[i] }'
cat <<EOF

## Checking the files

\`SHA256SUMS\` is signed with the [release signing key](https://github.com/$repo/blob/main/SECURITY.md#release-signing-key), and [artifact attestations](https://docs.github.com/en/actions/concepts/security/artifact-attestations) show that the files were built by this repository's release workflow. \`tegata init\` checks the signature of the providers it installs.

\`\`\`sh
v=$version p=linux_x86_64   # or linux_aarch64, darwin_x86_64, darwin_aarch64
gh release download v\$v -R $repo -p "tegata_\${v}_\${p}" -p SHA256SUMS -p SHA256SUMS.minisig
minisign -V -P $public_key -m SHA256SUMS
grep " tegata_\${v}_\${p}\$" SHA256SUMS | shasum -a 256 -c
gh attestation verify "tegata_\${v}_\${p}" -R $repo
\`\`\`
EOF
# The first release has no earlier version to compare with
if [ -n "$compare" ] && [[ $compare != */releases/tag/* ]]; then
  printf '\n**All changes:** %s\n' "$compare"
fi
