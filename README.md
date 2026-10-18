# tegata

**Secrets lifecycle as code for cloud secret stores.** tegata creates secrets (random keys, API tokens for services such as Cloudflare and Tailscale, database passwords), stores them in a cloud secret store, gives each server read access to exactly the secrets its templates use, rotates them, cleans up the old ones, and fills in configuration templates on the servers.

Everything is declared in one YAML file. No server to run, no Kubernetes required.

> **Status:** early. tegata is used in production by its author, but the configuration format and the provider protocol may still change before 1.0. The only store today is Google Cloud Secret Manager.

## Why

Cloud secret stores and Vault-style servers are good at *storing* secrets. Rotating secrets that belong to *other services* is usually left to you: write a rotation function per service, or do it by hand in each dashboard. tegata puts the whole lifecycle in one place:

- **Create:** a resource names a provider (`random`, `cloudflare`, `tailscale`, ...) and its settings. `tegata apply` creates what does not exist yet.
- **Store and grant:** values go to Secret Manager. Each role (a server, a service account) can read exactly the secrets whose names appear in its templates, and nothing else.
- **Rotate and clean up:** `tegata rotate` makes a new generation; after the servers pick it up, `tegata prune` deletes what the provider created for older generations (for example, the old API token) and disables the old versions.
- **Fill in:** on a server, `tegata render` replaces `__NAME__` in templates with the stored values.

Providers are separate executables that speak JSON over stdin and stdout, so you can add one in any language without changing tegata.

## Quick look

```yaml
# tegata.yaml
store:
  gcp:
    project: my-project
    location: us-central1          # where secret replicas live (user-managed replication)

roles:                             # who reads what: decided by the names in the templates
  app:
    member: serviceAccount:app-vm@my-project.iam.gserviceaccount.com
    templates: ./templates/app     # a directory of files with __NAME__ placeholders

providers:                         # external providers (random, manual and value are built in)
  cloudflare:
    command: tegata-provider-cloudflare
    config: { account_id: 0123456789abcdef }
    credentials:                   # never write secrets in the YAML
      api_token: { env: TEGATA_CLOUDFLARE_TOKEN }

resources:
  session_key:
    provider: random
    config: { bytes: 32 }
    secrets: { SESSION_KEY: value }
    previous: PREVIOUS_SESSION_KEY # rotate copies the current value here first
  releases_read:
    provider: cloudflare
    type: api_token
    config:
      name: releases read
      policies:
        - permissions: [Workers R2 Storage Bucket Item Read]
          r2_buckets: [releases]
    secrets:                       # secret name: provider output
      RELEASES_ACCESS_KEY_ID: id
      RELEASES_SECRET_ACCESS_KEY: r2_secret_access_key
```

```text
# templates/app/app.env
SESSION_KEY=__SESSION_KEY__
PREVIOUS_SESSION_KEY=__PREVIOUS_SESSION_KEY__
RELEASES_ACCESS_KEY_ID=__RELEASES_ACCESS_KEY_ID__
RELEASES_SECRET_ACCESS_KEY=__RELEASES_SECRET_ACCESS_KEY__
```

```sh
tegata check                 # names in templates vs. resources, providers
tegata apply                 # containers, read access, missing values
tegata status                # generations, enabled versions, readers

# on the server (no YAML needed; on GCP VMs the project comes from the metadata server)
tegata render templates/app /etc/app
```

Rotating a key:

```sh
tegata rotate session_key    # new generation; the old value moves to PREVIOUS_SESSION_KEY
# ... re-run `tegata render` on the servers and restart the services ...
tegata settle session_key    # PREVIOUS_SESSION_KEY = SESSION_KEY (the old value is no longer accepted)
tegata prune session_key     # disable the old versions
```

## Commands

| Command | What it does |
| --- | --- |
| `tegata check` | Checks the configuration, the providers, and that every `__NAME__` in the templates is made by some resource |
| `tegata status` | Shows each resource and secret: generations, enabled versions, which roles read it. Also lists tegata secrets that are no longer in the YAML |
| `tegata apply [--yes \| --dry-run]` | Creates secret containers, labels and read access; creates values that do not exist yet; re-reads `data` resources |
| `tegata rotate [--yes \| --dry-run] <resource>...` | Creates a new generation (copying the current value to `previous` first, if set) |
| `tegata settle <resource>...` | Makes `previous` equal to the current value (the end of a rotation) |
| `tegata prune [<resource>...]` | Deletes what providers created for older generations and disables those versions. Versions are disabled, not destroyed |
| `tegata set <resource>` | Puts a value by hand (hidden input on a terminal, otherwise stdin). For `manual` resources, or to restore a value |
| `tegata import <role> <dir>` | Fills secrets that have no value from files you filled in by hand before using tegata |
| `tegata render [--project <p>] <templates> <out>` | Fills `__NAME__` (and `__NAME\|sha256__`, the SHA-256 of the value) in each template file. Names that are missing or unreadable stay as they are and are listed |

The configuration file is `tegata.yaml`, or `-f <file>` / `TEGATA_FILE`.

**Values that stop working at once.** Some things can hold only one value: a database user's password, a Cloudflare Tunnel's secret. Creating a new one breaks new connections until the servers are updated. Providers declare these types, and tegata asks before creating them (`--yes` to skip, `--dry-run` to only show).

## Providers

| Provider | Types | What it makes |
| --- | --- | --- |
| `random` (built in) | | Random bytes as hex (`bytes`, default 32) |
| `manual` (built in) | | A value you type in (`tegata set`) |
| `value` (built in) | data | Fixed, non-secret values written in the YAML |
| `tegata-provider-cloudflare` | `api_token`, `tunnel_token` | Account API tokens (also usable as R2 keys), Tunnel tokens |
| `tegata-provider-tailscale` | `auth_key` | Auth keys for joining a tailnet |
| `tegata-provider-aws` | `ses_smtp_credentials` | Amazon SES SMTP credentials (an access key for an IAM user) |
| `tegata-provider-gcp` | `sql_user` | Cloud SQL built-in user passwords |
| `tegata-provider-terraform` | `outputs` (data) | Values read from Terraform outputs |

Writing a provider: see [the provider protocol](docs/provider-protocol.md).

## Install

Build from source (Rust 1.87 or later):

```sh
cargo install --path crates/tegata
cargo install --path crates/tegata-provider-cloudflare   # and any other providers you use
```

Prebuilt binaries and `tegata init` (install the providers a configuration needs, pinned by a lock file, like `terraform init`) are planned.

## License

Apache License 2.0. See [LICENSE](LICENSE).
