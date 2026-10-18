# Releasing providers

`tegata init` installs providers from GitHub releases. This page describes what a release must contain, so you can publish your own providers (the official ones are released the same way).

## What a release contains

A release is tagged `v<version>` (semantic versioning, for example `v0.3.1`) and has these assets:

| Asset | Content |
| --- | --- |
| `tegata-provider-<name>_<version>_<os>_<arch>` | The executable, not archived. `<os>` is `linux` or `darwin`, `<arch>` is `x86_64` or `aarch64`. `<name>` is the provider's name in the users' YAML |
| `SHA256SUMS` | One line per executable: `<sha256 hex>  <file name>` (the output of `sha256sum` or `shasum -a 256`) |
| `SHA256SUMS.minisig` | The [minisign](https://jedisct1.github.io/minisign/) signature of `SHA256SUMS` |

One release may hold several providers (the official release holds all of them). Draft releases and tags that are not `v<semver>` are ignored.

Users install with:

```yaml
providers:
  example:
    source: github.com/<owner>/<repo>
    version: "^0.3"
    public_key: "<your minisign public key>"
```

## Signing key

Make the key once:

```sh
minisign -G -p tegata.pub -s ~/.minisign/tegata.key
```

- Keep the secret key on your own machine, protected by its password. Do not put it in CI secrets or in the repository.
- Publish the public key (the second line of `tegata.pub`) in your README, so users can copy it into their YAML.
- If you lose the secret key or it leaks, make a new one, announce the new public key, and tell users to update `public_key`. tegata refuses releases signed by a key other than the one in the configuration.

## Steps

```sh
version=0.3.1
mkdir -p dist && cd dist

# 1. build each provider for each platform, and name the files
#    (for example with cargo and cross, or on one machine per platform)
cp ../target/x86_64-unknown-linux-musl/release/tegata-provider-example tegata-provider-example_${version}_linux_x86_64
cp ../target/aarch64-unknown-linux-musl/release/tegata-provider-example tegata-provider-example_${version}_linux_aarch64
cp ../target/aarch64-apple-darwin/release/tegata-provider-example tegata-provider-example_${version}_darwin_aarch64
# ...

# 2. checksums and signature (asks for the key's password)
shasum -a 256 tegata-provider-* > SHA256SUMS
minisign -S -s ~/.minisign/tegata.key -m SHA256SUMS

# 3. check before publishing
minisign -V -p ../tegata.pub -m SHA256SUMS
shasum -a 256 -c SHA256SUMS

# 4. publish
gh release create v${version} tegata-provider-* SHA256SUMS SHA256SUMS.minisig --title v${version}
```

You can check the release the way users will before announcing it: put the files in a mirror directory (`<dir>/github.com/<owner>/<repo>/<version>/`) and run `TEGATA_PROVIDER_MIRROR=<dir> tegata init` with a test configuration.

## How the official releases are made

The official releases of this repository (tegata and every provider in it) are built by GitHub Actions, not on a maintainer's machine, and signed by the maintainer:

1. Bump `version` in every `crates/*/Cargo.toml`, merge, and push a tag `v<version>`.
2. [`.github/workflows/release.yml`](../.github/workflows/release.yml) builds the binaries on GitHub-hosted runners (Linux x86_64 and aarch64 with musl; macOS on Apple Silicon for both architectures), adds [artifact attestations](https://docs.github.com/en/actions/concepts/security/artifact-attestations), writes `SHA256SUMS`, and puts everything in a **draft** release.
3. The maintainer runs `tools/sign-release.sh v<version>` on the machine that holds the signing key. It downloads the draft's files, checks them against `SHA256SUMS` and their attestations, signs `SHA256SUMS`, checks the signature with the official key, uploads `SHA256SUMS.minisig`, and publishes the release.

The signing key never reaches CI, so a compromised workflow can build files but cannot sign them.

## Never replace the files of a release

`tegata.lock` records the SHA-256 of every file. If the files of a published version change, `tegata init` stops with an error on every machine that locked that version. To fix a broken release, publish a new version.
