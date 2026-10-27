# Configuration reference

tegata reads one YAML file: `tegata.yaml` in the current directory, or the file given with `-f <file>` or `TEGATA_FILE`. Relative paths in it are relative to the directory of the file. Unknown keys are errors, so typos do not go unnoticed.

```yaml
stores: { ... }     # where secrets are stored (required; one store for now)
identities: { ... } # who reads secrets (optional)
providers: { ... }  # external providers (optional; random, manual and value are built in)
resources: { ... }  # what to create, where to store it, and who reads it (required)
defaults: { ... }   # how old a generation may get, for every resource (optional)
destinations: { ... } # where copies of secrets are written, such as GitHub Actions secrets (optional)
environments: { ... } # staging, production, ...: what differs between them (optional)
```

A complete example that runs without a cloud account is in [examples/minimal](../../examples/minimal). Files written for tegata 0.1 use an older form; see [The old form](#the-old-form-and-tegata-migrate).

## `stores`

A name → a store. Resources refer to the store by this name. Only one store is supported for now. Set exactly one of `gcp` and `local` in it.

### `gcp`: Google Cloud Secret Manager

```yaml
stores:
  main:
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
- **Read access**: the members of the `roles/secretmanager.secretAccessor` role on each secret are set to exactly the members of its [readers](#resources). Bindings of other roles on the secret are left as they are.

Credentials, in this order:

1. `TEGATA_GCP_ACCESS_TOKEN`, if set (an OAuth access token)
2. On a Google Compute Engine VM: the VM's service account, from the metadata server
3. Otherwise: `gcloud auth print-access-token` (your own login)

To run `apply`, `rotate`, and `prune`, the account needs to create secrets, add and disable versions, update labels and annotations, and set IAM policies on secrets (for example `roles/secretmanager.admin` on the project). `render` needs only `roles/secretmanager.secretAccessor`, which `apply` grants to the readers.

### `local`: a directory

```yaml
stores:
  main:
    local:
      dir: ./store
```

Each secret is one JSON file in `dir`, written with mode 0600, holding all its versions in plain text. Use it to try tegata and in tests. It records read access but does not enforce it.

## `identities`

An identity is a name for the IAM members that read secrets: the service accounts of the servers that run `tegata render`, an application that reads Secret Manager itself, a CI workflow. Resources give read access by naming identities in `readers`; an identity grants nothing by itself.

```yaml
identities:
  app:
    - serviceAccount:app-vm@my-project.iam.gserviceaccount.com
    - serviceAccount:app-vm-2@my-project.iam.gserviceaccount.com
  edge: [serviceAccount:edge-vm@my-project.iam.gserviceaccount.com]
```

Each identity is a list of IAM members, at least one: `serviceAccount:...`, `user:...`, `group:...`.

Which templates a server renders is decided where you deploy it, not here. To check that an identity can read every name in the templates it renders, give both to [`tegata check`](commands.md#tegata-check): `tegata check edge ./templates/edge`.

## `providers`

External providers are executables that tegata runs once per operation (see [the provider protocol](provider-protocol.md)). The built-in providers `random`, `manual`, and `value` need no entry here.

```yaml
providers:
  cloudflare:
    source: github.com/k-kawa/tegata
    version: "^0.4"
    config:
      account_id: 0123456789abcdef
    credentials:
      api_token: { env: TEGATA_CLOUDFLARE_TOKEN, optional: true }
```

| Key | Description |
| --- | --- |
| `command` | The executable. With a `/`, a path relative to the YAML file; otherwise looked up in `PATH`, then next to the `tegata` executable. Set either `command` or `source` |
| `source` | A GitHub repository whose releases hold the provider, `github.com/<owner>/<repo>`. Installed by `tegata init` (see [Installing providers](../../README.md#installing-providers)) |
| `version` | With `source` (required): a version requirement in Cargo syntax, such as `^1.2`, `~1.2.3`, `=1.2.3`, `>=1, <2`, or `*`. A bare `1.2` means `^1.2` |
| `public_key` | With `source`: the minisign public key that signs its releases. Not needed for `github.com/k-kawa/tegata`, whose key is built in |
| `config` | Settings passed to every operation as `provider_config` (account IDs and the like; never secrets). See [providers](providers.md) for what each provider takes |
| `credentials` | Secrets the provider needs to call its API, by name. See below |

Each credential:

| Key | Description |
| --- | --- |
| `env` | The environment variable to read it from |
| `from` | A [credential source](#credential_sources) to read it from: `{ source, secret, version }` for a `gcp` source (`version` defaults to the latest enabled one), `{ source, ref }` for a `command` source |
| `prompt` | The question when tegata asks for it (default: `<provider> <name>`) |
| `optional` | If `true` and neither the variable nor `from` gives a value, tegata leaves the credential out instead of asking, and the provider decides what to do (for example, use a CLI login). Default `false` |

tegata looks in this order, once per run: the variable `env` (when set and not empty), then `from`, then a hidden prompt on the terminal (an error outside one), unless the credential is `optional`. So `env` can override `from` for a one-off. Credentials are kept in memory only, and never written, logged, or shown in errors.

## `credential_sources`

Where providers' admin credentials come from: the tokens that let a provider make other tokens. They are kept apart from [`stores`](#stores), because whoever administers the store's project can grant themselves access to anything in it.

```yaml
credential_sources:
  admin:
    gcp: { project: myorg-admin }        # Secret Manager in another project
  op:
    command: [op, read]                  # any command; the reference is added as its last argument

providers:
  cloudflare:
    credentials:
      api_token:
        from: { source: admin, secret: cloudflare-admin-token }
        env: TEGATA_CLOUDFLARE_TOKEN     # wins when set
  tailscale:
    credentials:
      client_secret: { from: { source: op, ref: "op://infra/tailscale/oauth-secret" } }
```

| Kind | Reads |
| --- | --- |
| `gcp: { project }` | Secret Manager in that project, with the same Google credentials as the store (`TEGATA_GCP_ACCESS_TOKEN`, the VM's service account, or `gcloud`). The account needs `roles/secretmanager.secretAccessor` on the secrets it reads there, and nothing else |
| `command: [program, args...]` | Runs the program, without a shell, with the credential's `ref` as its last argument, and takes its standard output (one trailing newline removed). It gets the terminal, so tools that ask to be unlocked (1Password) work, and must finish within 60 seconds. A failure reports the exit status, never the output. A program with a `/` is relative to this file. For example: 1Password (`[op, read]`), Vault (`[vault, kv, get, -field=token]`), AWS Secrets Manager (`[aws, secretsmanager, get-secret-value, --query, SecretString, --output, text, --secret-id]`), `[pass, show]` |

Each source sets exactly one kind. How to keep admin credentials and run tegata in CI without long-lived keys: [Admin credentials and CI](../guides/admin-credentials.md).

## `resources`

A resource is one thing a provider makes, the secrets that hold its outputs, and who reads them.

```yaml
resources:
  session_key:
    provider: random
    config: { bytes: 32 }
    stores:
      main:
        - { name: SESSION_KEY, readers: [app] }
    previous: { name: PREVIOUS_SESSION_KEY }
  releases_read:
    provider: cloudflare
    type: api_token
    config: { ... }
    stores:
      main:
        - { output: id, name: RELEASES_ACCESS_KEY_ID, readers: [app] }
        - { output: r2_secret_access_key, name: RELEASES_SECRET_ACCESS_KEY, readers: [app, edge] }
```

The resource's name (`session_key`) may use lowercase letters, digits, `-`, and `_`, up to 63 characters (it is stored as a label value).

| Key | Description |
| --- | --- |
| `provider` | `random`, `manual`, `value`, or a name under `providers` |
| `type` | The provider's resource type (`api_token`, ...). Leave it out for the built-in providers |
| `config` | Settings for this resource, passed to the provider as `config` |
| `stores.<store>` | The secrets in that store: a list of entries, at least one (see below) |
| `max_age` | Optional. The longest a generation may live before it should be rotated: a number and `s`, `m`, `h` or `d` (`90d`). `tegata status` marks older generations, and `tegata check --stale` fails on them (see [Rotation age](#rotation-age)). Default: `defaults.max_age`. Not for `value` resources, which are read, not made |
| `previous` | Optional. `{ name: <secret> }`: a secret in the same store that keeps the value before the last rotation, so services can accept both the old and the new value for a while (see [Rotating secrets](../guides/rotating-secrets.md#keys-your-service-checks-rotate-settle-prune)). It holds the value of the resource's first secret by name order, and is read by the same readers. When a resource needs it: [When a secret needs `previous`](../concepts/previous-and-settle.md) |

Each entry under `stores.<store>`:

| Key | Description |
| --- | --- |
| `output` | The provider output stored in this secret. Default `value`, which is what `random`, `manual` and `cloudflare/tunnel_token` return. See [providers](providers.md) for the outputs of each type |
| `name` | The secret's name in the store |
| `readers` | Identities that can read it. Default: none. Read access is exactly the members of these identities: adding one and running `tegata apply` grants access, removing it and running `apply` takes it away |

Secret names may use letters, digits, `-`, and `_`, up to 255 characters. To use a secret in a template, its name must start with an uppercase letter and contain only uppercase letters, digits, and `_` (for example `SESSION_KEY`). Each secret belongs to exactly one resource.

**Renaming a resource** keeps its secrets and their values: when a secret is labeled with a resource that is no longer in the YAML, `apply` moves it to the resource that now lists it. A secret labeled with another resource that still exists is an error.

### Built-in providers

| Provider | Config | Outputs |
| --- | --- | --- |
| `random` | `bytes`: 8 to 256, default 32 | Random bytes from the operating system, as lowercase hex (so 32 bytes become 64 characters). Every output name gets the same value, so `output` can be left out |
| `manual` | none | A value you put with `tegata set <resource>`. `apply` creates the secret but not the value |
| `value` | A map of output name → string | The strings themselves. For non-secret values that templates need next to secrets (a sender address, a bucket name). Read again on every `apply` |

## `defaults`

Settings for every resource that does not set its own.

```yaml
defaults:
  max_age: 180d     # resources without their own max_age
  warn_before: 14d  # how long before max_age (or expiry) a generation shows as due
```

| Key | Description |
| --- | --- |
| `max_age` | Optional. The `max_age` of resources that do not set one. Applies to resources that make their values (`random`, `manual`, and managed provider types); `data` resources never go stale |
| `warn_before` | How long before `max_age`, or before the provider's expiry, a generation is shown as due. Default `14d` |

## `environments`

Environments (staging, production, ...) share one configuration and differ only where they must: the store, the members of each identity, provider settings and credentials, resource settings, and destinations. Who reads what (`readers`) is the same in every environment.

Each environment's overrides are written in a file of their own, or inline; both can be mixed.

```yaml
# tegata.yaml: what every environment shares
stores:
  main:
    gcp: { location: asia-northeast1 }   # the project comes from the environment
identities:
  app: []                                # the members come from the environment
environments:
  staging: envs/staging.yaml             # a file, relative to this one
  production:                            # or inline
    confirm: true
    stores: { main: { gcp: { project: myapp-prod } } }
    identities: { app: [serviceAccount:app@myapp-prod.iam.gserviceaccount.com] }
    resources: { db_password: { config: { instance: prod-main } } }
resources:
  db_password:
    provider: gcp
    type: sql_user
    config: { user: app }                # the instance comes from the environment
    stores:
      main:
        - { output: password, name: DATABASE_PASSWORD, readers: [app] }
```

```yaml
# envs/staging.yaml: only what differs in staging
stores: { main: { gcp: { project: myapp-staging } } }
identities: { app: [serviceAccount:app@myapp-staging.iam.gserviceaccount.com] }
resources: { db_password: { config: { instance: staging-main } } }
```

Choose the environment with `-e <name>` (or `TEGATA_ENV`). When the configuration has environments, every command but `render`, `init`, `check` and `env` needs one; there is no default. `tegata env <name>` prints an environment's configuration with its overrides merged in.

**What an environment can set.** Its overrides are merged over the shared part: maps key by key, while lists and other values are replaced.

| Key | Description |
| --- | --- |
| `stores.<store>` | The store, or part of it (here, the project) |
| `identities.<identity>` | The members. The identity must be declared in the shared part; declare it with `[]` when every environment gives its members |
| `providers.<provider>.config`, `.credentials` | Provider settings and credentials (for example, another secret or variable for production's admin token). The provider must be in the shared part |
| `credential_sources` | Where admin credentials come from (for example, another project for production) |
| `resources.<resource>.config` | Resource settings. The resource must be in the shared part: environments cannot add or remove resources |
| `destinations` | Destinations, merged by name |
| `confirm` | `true`: commands that change the store ask first (below). Only in an environment |

Anything else is an error that names the environment's file. After merging, the environment must be complete: a store, and at least one member in every identity.

**Paths** in an environment's file, such as a Terraform `dir`, are relative to the main configuration file, like every other path in it.

**`confirm: true`** makes `apply`, `rotate`, `settle`, `prune`, `set` and `import` say what they will do in that environment and ask for the environment's name before changing anything. `--yes` skips the question, for scripts and CI. Outside a terminal, without `--yes`, they refuse; `set` with the value on standard input therefore needs `--yes`. `--dry-run` and read-only commands never ask.

```text
$ tegata -e production rotate session_key
production: make a new generation of session_key
Type the environment name to continue: production
```

**Keeping environments apart.** Secrets get the label `tegata-env=<name>`, and a command refuses to touch a secret labeled with another environment. `tegata check` without `-e` checks every environment, and reports two environments that use the same Google Cloud project or give access to the same IAM member. `tegata.lock` is shared: every environment uses the same providers.

Environments go with the current form (`stores`, `identities`); a file in the [old form](#the-old-form-and-tegata-migrate) is migrated first.

## `destinations`

A destination receives copies of secrets: tegata writes the latest value there whenever it changes. Today the only kind is **GitHub Actions secrets**, of a repository or of one of its environments.

```yaml
destinations:
  releases-ci:
    github: { repo: my-org/my-repo }
    secrets: [RELEASES_ACCESS_KEY_ID, RELEASES_SECRET_ACCESS_KEY]
  deploy:
    github: { repo: my-org/my-repo, environment: production }
    secrets:
      DEPLOY_TOKEN: deploy_token_value   # name on GitHub: secret name in tegata
```

| Key | Description |
| --- | --- |
| `github.repo` | `owner/repo` |
| `github.environment` | Optional. Write the secrets of this environment instead of the repository's. Environment secrets can be limited with protection rules (required reviewers, branches), and override repository secrets of the same name |
| `secrets` | The secrets to deliver: a list (the same names on GitHub), or a map of name on GitHub → secret name in tegata. Any secret of a resource, including `previous` secrets |

GitHub secret names may use letters, digits, and `_`, must not start with a digit or `GITHUB_`, and ignore case; tegata checks this when it loads the file. Rename secrets whose names do not fit with a map.

**The store stays the source.** GitHub secrets can be written but never read back, and keep no versions, so a destination only receives copies. tegata records each delivery on the secret in the store (the annotation `tegata-delivered`: the version it wrote, where, and GitHub's update time right after writing).

| Command | What happens at destinations |
| --- | --- |
| `apply` | Writes every secret whose latest version has not been delivered, or that was changed or removed on GitHub since tegata wrote it (the store's value wins, and tegata says so). A secret of the same name that tegata did not write is overwritten, also with a note. Deliveries the configuration no longer lists (a secret taken out of a destination, renamed there, or a whole destination removed) are removed from GitHub. Only the secrets this configuration declares, in this environment, are considered: see [removing a resource, and sharing a store](#removing-a-resource-and-sharing-a-store). Secrets tegata never wrote are never removed |
| `rotate`, `set`, `settle`, `import` | Write the changed values (and anything else not delivered yet). Workflows use the new value from their next run; nothing needs restarting |
| `prune` | Nothing (GitHub keeps only the latest value) |
| `status` | For each destination and secret: delivered (version), not delivered yet, changed or missing on GitHub. Also lists the other secrets there, which tegata does not touch, and, for an environment, names that are also repository secrets. For a managed secret the configuration does not declare, where it is still delivered |
| `apply --dry-run` | Shows what would be written and removed |

A secret labeled with another environment is never delivered: its value must not reach this environment's destinations. The command ends with an error naming it.

### Removing a resource, and sharing a store

Several configurations may use the same store, for example two repositories, each with its own `tegata.yaml`, keeping their secrets in one Google Cloud project. To keep them from touching each other, `apply` only removes deliveries of secrets that **this configuration declares** (in a resource, including `previous`) and that are not labeled with another environment. In the store, a secret of another configuration and a secret whose resource you removed look the same, so tegata leaves the deliveries of both alone.

To remove a resource and its copies on GitHub, do it in two steps:

1. Take its secrets out of `destinations`, and run `apply`: the copies are removed from GitHub.
2. Remove the resource. Its secrets stay in the store, and `status` lists them as `not in the config`; delete them by hand when nothing reads them.

If you removed the resource first, `status` shows where its secrets are still delivered (`delivered to … (…; apply leaves it)`). Put the resource back and follow the steps above, or delete those GitHub secrets by hand.

Configurations that share a store should not declare the same secret names: each secret has one owner.

If writing fails (no permission, GitHub unreachable), the value is still in the store, the command ends with an error naming what was not delivered, and the next `apply` delivers it.

**Token:** `TEGATA_GITHUB_TOKEN`, or else the login of the `gh` command-line tool (`gh auth token`). A fine-grained token needs the repository permission "Secrets" (for repository secrets) or "Environments" (for environment secrets), with write access, on the repositories it delivers to. It is a separate variable from `GITHUB_TOKEN`, which `tegata init` uses to read releases, so that a token that can write secrets is not used by accident.

If you run on Google Cloud, consider also letting workflows read Secret Manager directly through Workload Identity Federation: make the workflow's identity a member of an [identity](#identities) that reads the secret, and no copy of the secret is kept on GitHub.

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

The steps of a rotation, for each kind of value: [Rotating secrets](../guides/rotating-secrets.md).

### Rotation age

Each generation records when it was made (`created`, in the annotation of the resource's first secret), and when its value expires if the provider reports it (`expires`; for example `tailscale/auth_key`, and `cloudflare/api_token` with `expiry_seconds`). Generations made before tegata recorded the time are as old as the version they added, as the store reports it.

| Mark in `tegata status` | Meaning | `tegata check --stale` |
| --- | --- | --- |
| `DUE` | Within `warn_before` of `max_age`, or of the expiry | Warning |
| `ROTATE NOW` | Older than `max_age` | Fails |
| `EXPIRED` | Past the expiry the provider reported: the value no longer works | Fails |

A rotation is `tegata rotate <resource>`, or `tegata set <resource>` for `manual` resources; the new generation's age starts again. `data` resources (`value`, `terraform`) have no generations and never go stale. `max_age` and `defaults` are the same in every environment. Running the check on a schedule: [Rotating on a schedule](../guides/rotating-secrets.md#rotating-on-a-schedule).

**Values that stop working at once.** Some providers declare types that can hold only one value (a database user's password, a Cloudflare Tunnel's secret): making a new one breaks the old value immediately. Before `apply` or `rotate` makes such a value, tegata shows what will break and who reads it, and asks you to type `yes`. `--yes` skips the question (for scripts); `--dry-run` only shows what would be made. Outside a terminal, without `--yes`, tegata refuses.

## The old form and `tegata migrate`

Files written for tegata 0.1 use `store`, `roles` and `resources.*.secrets`. tegata still reads them, with a warning, and read access then comes from the templates, as it did:

```yaml
store: { local: { dir: ./store } }
roles:
  app:
    member: serviceAccount:app-vm@my-project.iam.gserviceaccount.com
    templates: ./templates/app      # app reads every secret named in these templates
resources:
  session_key:
    provider: random
    secrets: { SESSION_KEY: value } # secret name: provider output
    previous: PREVIOUS_SESSION_KEY
```

A file uses one form or the other; mixing them is an error. `tegata migrate` prints the file in the current form, with each secret's `readers` filled in from the templates (what `apply` grants today), so `apply` afterwards changes nothing in the store. Comments are not kept, and neither are the roles' `templates`: `migrate` prints on standard error the `tegata check <identity> <templates>` commands that compare them, for your CI or deploy scripts. In the current form a `previous` secret is read by the readers of the value it copies; if the templates give them different readers, `migrate` stops and names them, so you can make the templates agree first.

```sh
tegata migrate > tegata.new.yaml
diff tegata.yaml tegata.new.yaml
mv tegata.new.yaml tegata.yaml
tegata check
tegata apply --dry-run
```

## `tegata.lock`

Written by `tegata init` next to the YAML file, and meant to be committed. For each provider with a `source`, it records the chosen version, the version requirement it was chosen for, the public key that signed it, and the SHA-256 of the executable for every platform in the release. See [Installing providers](../../README.md#installing-providers).

## Environment variables

| Variable | Use |
| --- | --- |
| `TEGATA_FILE` | The configuration file (instead of `-f`) |
| `TEGATA_ENV` | The environment (instead of `-e`) |
| `TEGATA_GCP_ACCESS_TOKEN` | An OAuth access token for Secret Manager (and for the `gcp` provider) |
| `TEGATA_CACHE_DIR` | Where `tegata init` installs providers (default `$XDG_CACHE_HOME/tegata`, or `~/.cache/tegata`) |
| `TEGATA_PROVIDER_MIRROR` | Install providers from a local directory, laid out as `<dir>/<source>/<version>/<file>` |
| `GITHUB_TOKEN` | Sent to GitHub by `tegata init` (optional; needed for private repositories). Without it, only the list of releases uses the API, and files come from the public download URLs |
| `TEGATA_GITHUB_TOKEN` | The token for writing GitHub Actions secrets ([destinations](#destinations)); without it, `gh auth token` |
| `TEGATA_GITHUB_API` | The GitHub API URL, for `tegata init` and destinations (default `https://api.github.com`) |
| `TEGATA_GITHUB_DOWNLOAD` | The base of the public download URLs for `tegata init` (default `https://github.com`) |

Variables named in `credentials` are read too. Variables that only tests use (to point tegata and the providers at fake servers) are listed in the source code and are not part of the interface.
