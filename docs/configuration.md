# Configuration reference

tegata reads one YAML file: `tegata.yaml` in the current directory, or the file given with `-f <file>` or `TEGATA_FILE`. Relative paths in it are relative to the directory of the file. Unknown keys are errors, so typos do not go unnoticed.

```yaml
store: { ... }      # where secrets are stored (required)
roles: { ... }      # who reads which secrets (optional)
providers: { ... }  # external providers (optional; random, manual and value are built in)
resources: { ... }  # what to create (required)
```

A complete example that runs without a cloud account is in [examples/minimal](../examples/minimal).

## `store`

Set exactly one of `gcp` and `local`.

### `store.gcp`: Google Cloud Secret Manager

```yaml
store:
  gcp:
    project: my-project
    location: us-central1
```

| Key | Description |
| --- | --- |
| `project` | The Google Cloud project that holds the secrets |
| `location` | The region where new secrets keep their replica (user-managed replication with one replica). Secrets that already exist are not changed |

What tegata writes to Secret Manager:

- **Labels** on every secret it manages: `managed-by=tegata` and `tegata-resource=<resource name>`. `tegata status` uses them to list secrets that are no longer in the YAML.
- **Annotations** on the first secret of each resource (by name order): `tegata-gen-<n>`, one per generation that has not been pruned, holding the version numbers it added and the provider's `state`. Provider `state` is plain text here, which is why providers must never put secret values in it.
- **Read access**: the members of the `roles/secretmanager.secretAccessor` role on each secret are set to exactly the roles that use it (see [roles](#roles)). Bindings of other roles on the secret are left as they are.

Credentials, in this order:

1. `TEGATA_GCP_ACCESS_TOKEN`, if set (an OAuth access token)
2. On a Google Compute Engine VM: the VM's service account, from the metadata server
3. Otherwise: `gcloud auth print-access-token` (your own login)

To run `apply`, `rotate`, and `prune`, the account needs to create secrets, add and disable versions, update labels and annotations, and set IAM policies on secrets (for example `roles/secretmanager.admin` on the project). `render` needs only `roles/secretmanager.secretAccessor`, which `apply` grants to the roles.

### `store.local`: a directory

```yaml
store:
  local:
    dir: ./store
```

Each secret is one JSON file in `dir`, written with mode 0600, holding all its versions in plain text. Use it to try tegata and in tests. It records read access but does not enforce it.

## `roles`

A role is a reader, such as a server or a service account, and the templates it fills in.

```yaml
roles:
  app:
    member: serviceAccount:app-vm@my-project.iam.gserviceaccount.com
    templates: ./templates/app
```

| Key | Description |
| --- | --- |
| `member` | Who gets read access, in IAM member form (`serviceAccount:...`, `user:...`, `group:...`) |
| `templates` | A directory of template files. Only the files directly in it count; subdirectories are ignored. Symbolic links to files are followed, so roles can share a template |

**Read access follows the templates.** A role can read a secret exactly when one of its template files contains that secret's name (`__NAME__` or `__NAME|sha256__`). Adding a name to a template and running `tegata apply` grants access; removing it and running `apply` again takes it away. A secret no template uses has no readers.

`tegata check` reports names in templates that no resource makes.

## `providers`

External providers are executables that tegata runs once per operation (see [the provider protocol](provider-protocol.md)). The built-in providers `random`, `manual`, and `value` need no entry here.

```yaml
providers:
  cloudflare:
    source: github.com/k-kawa/tegata
    version: "^0.1"
    config:
      account_id: 0123456789abcdef
    credentials:
      api_token: { env: TEGATA_CLOUDFLARE_TOKEN, optional: true }
```

| Key | Description |
| --- | --- |
| `command` | The executable. With a `/`, a path relative to the YAML file; otherwise looked up in `PATH`, then next to the `tegata` executable. Set either `command` or `source` |
| `source` | A GitHub repository whose releases hold the provider, `github.com/<owner>/<repo>`. Installed by `tegata init` (see [Installing providers](../README.md#installing-providers)) |
| `version` | With `source` (required): a version requirement in Cargo syntax, such as `^1.2`, `~1.2.3`, `=1.2.3`, `>=1, <2`, or `*`. A bare `1.2` means `^1.2` |
| `public_key` | With `source`: the minisign public key that signs its releases. Not needed for `github.com/k-kawa/tegata`, whose key is built in |
| `config` | Settings passed to every operation as `provider_config` (account IDs and the like; never secrets). See [providers](providers.md) for what each provider takes |
| `credentials` | Secrets the provider needs to call its API, by name. See below |

Each credential:

| Key | Description |
| --- | --- |
| `env` | The environment variable to read it from |
| `prompt` | The question when tegata asks for it (default: `<provider> <name>`) |
| `optional` | If `true` and the variable is not set, tegata leaves the credential out instead of asking, and the provider decides what to do (for example, use a CLI login). Default `false` |

If the variable is not set (or is empty) and the credential is not optional, tegata asks for it on the terminal with hidden input, once per run. Outside a terminal, that is an error. Credentials are never written anywhere.

## `resources`

A resource is one thing a provider makes, and the secrets that hold its outputs.

```yaml
resources:
  session_key:
    provider: random
    config: { bytes: 32 }
    secrets: { SESSION_KEY: value }
    previous: PREVIOUS_SESSION_KEY
```

The resource's name (`session_key`) may use lowercase letters, digits, `-`, and `_`, up to 63 characters (it is stored as a label value).

| Key | Description |
| --- | --- |
| `provider` | `random`, `manual`, `value`, or a name under `providers` |
| `type` | The provider's resource type (`api_token`, ...). Leave it out for the built-in providers |
| `config` | Settings for this resource, passed to the provider as `config` |
| `secrets` | Secret name → the provider output stored in it. At least one |
| `previous` | Optional. A secret that keeps the value before the last rotation, so services can accept both the old and the new value for a while (see [Rotation](#rotation)). It holds the value of the resource's first secret by name order |

Secret names may use letters, digits, `-`, and `_`, up to 255 characters. To use a secret in a template, its name must start with an uppercase letter and contain only uppercase letters, digits, and `_` (for example `SESSION_KEY`). Each secret belongs to exactly one resource.

**Renaming a resource** keeps its secrets and their values: when a secret is labeled with a resource that is no longer in the YAML, `apply` moves it to the resource that now lists it. A secret labeled with another resource that still exists is an error.

### Built-in providers

| Provider | Config | Outputs |
| --- | --- | --- |
| `random` | `bytes`: 8 to 256, default 32 | Random bytes from the operating system, as lowercase hex (so 32 bytes become 64 characters). Every output name gets the same value, so `secrets: { KEY: value }` works with any output name |
| `manual` | none | A value you put with `tegata set <resource>`. `apply` creates the secret but not the value |
| `value` | A map of output name → string | The strings themselves. For non-secret values that templates need next to secrets (a sender address, a bucket name). Read again on every `apply` |

## Templates

A template is a text file with placeholders:

| Placeholder | Becomes |
| --- | --- |
| `__NAME__` | The value of secret `NAME` |
| `__NAME\|sha256__` | The SHA-256 of the value, as lowercase hex (for services that store only a hash) |

`NAME` starts with an uppercase letter and contains only uppercase letters, digits, and `_`. Anything else between `__` and `__` is left alone, so `__init__` or `____` in a file are not placeholders.

`tegata render <templates> <out>` fills every file directly in `<templates>` with the latest enabled version of each secret, and writes the result to `<out>` with the same file names. `<out>` is created with mode 0700 and each file is written with mode 0600, through a temporary file and a rename, so readers never see half a file. Names that are missing in the store, or that the caller cannot read, stay as `__NAME__` and are listed in the output.

`render` reads only the store, not the YAML, and never runs providers. With Secret Manager it needs the project: `--project <project>`, or on a Compute Engine VM, the VM's own project. `--store-dir <dir>` reads a `local` store instead.

## Generations and rotation

Each time a managed resource makes new values, the values form a **generation**: one new version in each of the resource's secrets, plus the provider's `state` (what it needs to delete them later).

| Command | What changes |
| --- | --- |
| `apply` | For each managed resource whose secrets have no value yet, makes the first generation. For `data` resources (`value`, `terraform`), reads the outputs again; a changed output becomes a new version and the older versions are disabled |
| `rotate <resource>` | Makes a new generation. If the resource has `previous`, copies the current value there first |
| `settle <resource>` | Copies the current value to `previous`, so the old value is no longer accepted |
| `prune [<resource>]` | For every generation older than the latest: asks the provider to delete what it made (an API token, an access key), then disables that generation's versions. Also disables older versions of `previous`. Versions are disabled, never destroyed, so they can be enabled again in the store if needed |
| `set <resource>` | Puts a value by hand as a new generation. Generations put by hand (or by `import`) for an external provider are not deleted by `prune`, since tegata did not make them; prune tells you to clean them up by hand |

A typical rotation of a key that services check against both the current and the previous value:

```sh
tegata rotate session_key   # SESSION_KEY: new value; PREVIOUS_SESSION_KEY: the old one
# render on the servers and restart the services: they accept both values
tegata settle session_key   # PREVIOUS_SESSION_KEY: the new value too; the old one is no longer accepted
tegata prune session_key    # disable the old versions
```

A rotation of an API token that is replaced at once:

```sh
tegata rotate releases_read # a second token exists; the old one still works
# render on the servers and restart the services
tegata prune releases_read  # delete the old token on the provider
```

**Values that stop working at once.** Some providers declare types that can hold only one value (a database user's password, a Cloudflare Tunnel's secret): making a new one breaks the old value immediately. Before `apply` or `rotate` makes such a value, tegata shows what will break and who reads it, and asks you to type `yes`. `--yes` skips the question (for scripts); `--dry-run` only shows what would be made. Outside a terminal, without `--yes`, tegata refuses.

## `tegata.lock`

Written by `tegata init` next to the YAML file, and meant to be committed. For each provider with a `source`, it records the chosen version, the version requirement it was chosen for, the public key that signed it, and the SHA-256 of the executable for every platform in the release. See [Installing providers](../README.md#installing-providers).

## Environment variables

| Variable | Use |
| --- | --- |
| `TEGATA_FILE` | The configuration file (instead of `-f`) |
| `TEGATA_GCP_ACCESS_TOKEN` | An OAuth access token for Secret Manager (and for the `gcp` provider) |
| `TEGATA_CACHE_DIR` | Where `tegata init` installs providers (default `$XDG_CACHE_HOME/tegata`, or `~/.cache/tegata`) |
| `TEGATA_PROVIDER_MIRROR` | Install providers from a local directory, laid out as `<dir>/<source>/<version>/<file>` |
| `GITHUB_TOKEN` | Sent to GitHub by `tegata init` (private repositories, higher rate limits) |
| `TEGATA_GITHUB_API` | The GitHub API URL (default `https://api.github.com`) |

Variables named in `credentials` are read too. Variables that only tests use (to point tegata and the providers at fake servers) are listed in the source code and are not part of the interface.
