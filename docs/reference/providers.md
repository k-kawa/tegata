# Providers

The providers in this repository. Each is a separate executable, `tegata-provider-<name>`; install them with `tegata init` (a `source`) or put them on `PATH` (a `command`). See [configuration](configuration.md#providers) for the keys every provider entry has, and [configuration](configuration.md#built-in-providers) for the built-in `random`, `manual`, and `value`.

For each type:

- **Managed** types are created by `apply` and `rotate` and deleted by `prune`. **Data** types are only read, on every `apply`.
- **Outputs** are what you put in `output` of each entry under `stores.<store>` (see [resources](configuration.md#resources)). The examples store everything in a store named `main`, read by an identity named `app`.
- **Replaced at once** means that making a new value stops the previous one from working, so tegata asks before doing it (see [configuration](configuration.md#generations-and-rotation)).

| Provider | Type | Kind | Replaced at once | What it makes | What `prune` does |
| --- | --- | --- | --- | --- | --- |
| [`cloudflare`](#cloudflare) | `api_token` | managed | no | An account API token (also R2 S3 keys) | Deletes the older tokens |
| [`cloudflare`](#cloudflare) | `tunnel_token` | managed | yes | A new secret for an existing tunnel | Deletes nothing; refuses while connectors started before the rotation are connected |
| [`tailscale`](#tailscale) | `auth_key` | managed | no | An auth key for the tailnet | Deletes the older keys |
| [`aws`](#aws) | `ses_smtp_credentials` | managed | no | An IAM access key and the SES SMTP password derived from it | Deletes the older access keys |
| [`gcp`](#gcp) | `sql_user` | managed | yes | A Cloud SQL built-in user's password | Deletes nothing |
| [`terraform`](#terraform) | `outputs` | data | | Values read from a Terraform output | |

## cloudflare

```yaml
providers:
  cloudflare:
    source: github.com/k-kawa/tegata
    version: "^0.2"
    config:
      account_id: 0123456789abcdef0123456789abcdef
    credentials:
      api_token: { env: TEGATA_CLOUDFLARE_TOKEN, optional: true }
```

| Provider key | Description |
| --- | --- |
| `config.account_id` | The Cloudflare account |
| `credentials.api_token` | An API token that can create account API tokens (and, for `tunnel_token`, edit Cloudflare Tunnels). Optional: without it, the provider uses the login of Cloudflare's `cf` command-line tool, read from its configuration file (`~/Library/Preferences/cloudflare/config/default.json` on macOS, `$XDG_CONFIG_HOME/cloudflare/config/default.json` or `~/.config/cloudflare/config/default.json` elsewhere). When that login expires within two minutes, the provider runs `cf auth whoami` so that `cf` refreshes it |

### `api_token` (managed)

Creates an **account** API token (owned by the account, so it keeps working when the person who created it leaves).

```yaml
resources:
  releases_read:
    provider: cloudflare
    type: api_token
    config:
      name: releases read
      policies:
        - permissions: [Workers R2 Storage Bucket Item Read]
          r2_buckets: [releases]
        - permissions: ["DNS Write|DNS Edit"]
          zones: [example.com]
    stores:
      main:
        - { output: id, name: RELEASES_ACCESS_KEY_ID, readers: [app] }
        - { output: r2_secret_access_key, name: RELEASES_SECRET_ACCESS_KEY, readers: [app] }
```

| Config | Description |
| --- | --- |
| `name` | The token's name. The creation time is added (`releases read (2026-10-18 09:30 UTC)`), so the old and the new token can be told apart during a rotation |
| `policies` | A list of policies. Each has `permissions` (names of permission groups as the dashboard shows them; `"A\|B"` tries `A`, then `B`, for groups that were renamed) and at least one of `account: true` (the whole account), `r2_buckets` (bucket names), and `zones` (zone names) |

| Output | Description |
| --- | --- |
| `id` | The token's ID. Also the access key ID for R2's S3 API |
| `value` | The token |
| `r2_secret_access_key` | The SHA-256 of the token, which is the secret access key for R2's S3 API |

`prune` deletes the older tokens (a token that no longer exists counts as deleted).

### `tunnel_token` (managed, replaced at once)

Rotates the secret of an existing, remotely managed Cloudflare Tunnel and returns its token (what `cloudflared` uses to connect). Create the tunnel itself elsewhere, for example with Terraform.

```yaml
resources:
  tunnel:
    provider: cloudflare
    type: tunnel_token
    config: { tunnel: web }
    stores:
      main:
        - { name: TUNNEL_TOKEN, readers: [app] }
```

| Config | Description |
| --- | --- |
| `tunnel` | The tunnel's name (exactly one non-deleted tunnel must have it) |

| Output | Description |
| --- | --- |
| `value` | The tunnel token |

A tunnel has only one secret. After a rotation, `cloudflared` instances that are already connected stay connected, but new connections with the old token fail. `prune` deletes nothing on Cloudflare; instead it checks that no connector started before the rotation is still connected, and refuses (naming the connectors and where they connect from) until they have been restarted with the new token.

## tailscale

```yaml
providers:
  tailscale:
    source: github.com/k-kawa/tegata
    version: "^0.2"
    credentials:
      client_id: { env: TEGATA_TAILSCALE_CLIENT_ID }
      client_secret: { env: TEGATA_TAILSCALE_CLIENT_SECRET }
```

| Provider key | Description |
| --- | --- |
| `credentials.client_id`, `credentials.client_secret` | A Tailscale OAuth client that can write auth keys, with the tags the keys will carry |

### `auth_key` (managed)

Creates an auth key for adding devices to the tailnet.

```yaml
resources:
  join_key:
    provider: tailscale
    type: auth_key
    config:
      tags: [tag:server]
      reusable: true
    stores:
      main:
        - { output: key, name: TAILSCALE_AUTH_KEY, readers: [app] }
```

| Config | Description |
| --- | --- |
| `tags` | Required: keys made by OAuth clients must carry tags |
| `reusable` | Default `false` |
| `ephemeral` | Default `false` |
| `preauthorized` | Default `true` |
| `expiry_seconds` | Default 604800 (7 days) |
| `description` | Default: the resource's name |

| Output | Description |
| --- | --- |
| `key` | The auth key |

`prune` deletes the older keys (keys that were used up or expired count as deleted).

## aws

```yaml
providers:
  aws:
    source: github.com/k-kawa/tegata
    version: "^0.2"
```

The provider finds AWS credentials the way the AWS SDKs and CLI do: environment variables, `~/.aws/credentials` and `~/.aws/config`, an `aws sso login` session, and so on. The `aws` CLI itself is not needed. No `credentials` entry is used.

### `ses_smtp_credentials` (managed)

Creates an access key for an IAM user and derives the Amazon SES SMTP password from it ([how SES derives it](https://docs.aws.amazon.com/ses/latest/dg/smtp-credentials.html)). Create the IAM user yourself, with permission to send email through SES and nothing else.

```yaml
resources:
  smtp:
    provider: aws
    type: ses_smtp_credentials
    config: { user: ses-sender, region: us-west-2 }
    stores:
      main:
        - { output: username, name: SMTP_USERNAME, readers: [app] }
        - { output: password, name: SMTP_PASSWORD, readers: [app] }
```

| Config | Description |
| --- | --- |
| `user` | The IAM user |
| `region` | The SES region the password is for. Default `us-west-2` |
| `profile` | An AWS profile to use. Optional |

| Output | Description |
| --- | --- |
| `username` | The access key ID (the SMTP user name) |
| `password` | The SMTP password. The access key's secret itself is not returned |

`prune` deletes the older access keys. An IAM user can have at most two access keys, so prune after each rotation before rotating again.

## gcp

```yaml
providers:
  gcp:
    source: github.com/k-kawa/tegata
    version: "^0.2"
    config: { project: my-project }
```

Credentials: `TEGATA_GCP_ACCESS_TOKEN` if set, otherwise `gcloud auth print-access-token`.

| Provider key | Description |
| --- | --- |
| `config.project` | The Google Cloud project of the Cloud SQL instances |

### `sql_user` (managed, replaced at once)

Sets a random password on a Cloud SQL built-in user, creating the user if it does not exist.

```yaml
resources:
  db_password:
    provider: gcp
    type: sql_user
    config: { instance: main, user: app }
    stores:
      main:
        - { output: password, name: DATABASE_PASSWORD, readers: [app] }
```

| Config | Description |
| --- | --- |
| `instance` | The Cloud SQL instance |
| `user` | The user. It must be a built-in user (not an IAM user) |
| `bytes` | Random bytes in the password, written as hex. Default 24 |

| Output | Description |
| --- | --- |
| `password` | The password |
| `user` | The user name |

A user has only one password: open connections stay, new connections with the old password fail. `prune` deletes nothing (the old password already stopped working, and the user is never deleted, since it owns data).

## terraform

```yaml
providers:
  terraform:
    source: github.com/k-kawa/tegata
    version: "^0.2"
```

### `outputs` (data)

Reads one output of a Terraform configuration: `terraform -chdir=<dir> output -json <output>`. It only reads the state; it never plans or applies. The directory must be initialized (`terraform init`) and you need access to its state. `TEGATA_TERRAFORM` sets the `terraform` executable (default: `terraform` on `PATH`).

```yaml
resources:
  infra:
    provider: terraform
    type: outputs
    config: { dir: ../terraform, output: app_settings }
    stores:
      main:
        - { output: bucket, name: BUCKET_NAME, readers: [app] }
        - { output: db_host, name: DATABASE_HOST, readers: [app] }
```

| Config | Description |
| --- | --- |
| `dir` | The Terraform directory, relative to the YAML file |
| `output` | The name of an output whose value is a map. Its string, number, and boolean values become outputs; other values are an error |

The outputs are the keys of that map.
