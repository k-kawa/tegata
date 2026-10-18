# Changelog

All notable changes to tegata and the providers in this repository, which share one version. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/): before 1.0, a minor version (0.2 → 0.3) may change the configuration or the provider protocol, and says how to move.

## [Unreleased]

### Added

- **Environments** (#6): staging, production, and others share one configuration and differ only in the store, the members of each identity, provider settings and credentials, resource settings, and destinations. Each environment's overrides are in a file of their own (`environments: { production: envs/production.yaml }`) or inline. Choose one with `-e <name>` or `TEGATA_ENV`; `tegata env <name>` prints its merged configuration.
- `confirm: true` in an environment makes commands that change the store say what they will do there and ask for the environment's name. `--yes` skips the question, and is now accepted by `settle`, `prune`, `set` and `import` too.
- Secrets of an environment are labeled `tegata-env=<name>`, and tegata refuses to touch another environment's secrets. `tegata check` without `-e` checks every environment, and reports environments that share a Google Cloud project or an IAM member.

## [0.2.0] - 2026-10-19

0.2.0 changes the shape of `tegata.yaml`. Files written for 0.1 still work, with a warning, and `tegata migrate` prints them in the new shape.

### Changed

- **The configuration names its store, its identities, and the readers of each secret** (#13, #14). `store` becomes `stores` (a name → a store; one store for now), `roles` becomes `identities` (a name → a list of IAM members), and each resource lists its secrets under `stores.<store>` as `{ output, name, readers }`. Read access on each secret is exactly the members of its `readers`; templates no longer grant access. `previous` is written `{ name: ... }` and is read by the readers of the value it copies. `output` defaults to `value`.
- `tegata check` checks the configuration and the providers. `tegata check <identity> <templates>` also checks that the identity can read every name in the templates, and lists the secrets it can read but the templates do not use as warnings.
- `tegata import <templates> <dir>` takes the template directory the files were filled from, instead of a role.
- The documentation is organized into guides and reference, with a command reference (#10).

### Added

- `tegata migrate` prints the configuration in the new shape, with `readers` taken from the templates, so that `apply` afterwards changes nothing in the store. On standard error, it lists the `tegata check` commands that compare the old roles' templates.
- `tegata --help` lists the documentation, each command's help links to its section of the command reference, and an error is followed by a `see:` line naming the page that helps (#11).

### Moving from 0.1

```sh
tegata migrate > tegata.new.yaml   # also lists the tegata check commands for your templates
diff tegata.yaml tegata.new.yaml   # comments are not kept
mv tegata.new.yaml tegata.yaml
tegata apply --dry-run             # nothing to make
```

The provider protocol has not changed, so providers locked at 0.1 keep working. To move them to 0.2, set `version: "^0.2"` and run `tegata init --upgrade`.

## [0.1.1] - 2026-10-18

### Fixed

- `tegata init` stays under GitHub's limit for requests without a token: without `GITHUB_TOKEN`, it downloads release files from the public download URLs and uses the API only for the list of releases. With a token, each release is fetched once instead of once per file. A 403 or 429 without a token says how to set one (#3).

## [0.1.0] - 2026-10-18

The first release.

### Added

- The commands `init`, `check`, `status`, `apply`, `rotate`, `settle`, `prune`, `set`, `import`, and `render`.
- Stores: Google Cloud Secret Manager, and a local directory for trying tegata.
- The built-in providers `random`, `manual`, and `value`, and the providers `cloudflare` (`api_token`, `tunnel_token`), `tailscale` (`auth_key`), `aws` (`ses_smtp_credentials`), `gcp` (`sql_user`), and `terraform` (`outputs`).
- The provider protocol (version 1), and the `tegata-plugin` crate for writing providers in Rust.
- `tegata init` installs providers from GitHub releases: it checks the minisign signature of `SHA256SUMS` and records the SHA-256 of every platform's file in `tegata.lock`.
- Destinations: copies of secrets in the GitHub Actions secrets of a repository or an environment.
- Releases are built on GitHub-hosted runners with artifact attestations, and `SHA256SUMS` is signed by the maintainer.

[Unreleased]: https://github.com/k-kawa/tegata/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/k-kawa/tegata/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/k-kawa/tegata/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/k-kawa/tegata/releases/tag/v0.1.0
