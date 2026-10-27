# Command reference

Every command of `tegata`, its options, and what it prints. The outputs below were captured on [examples/minimal](../../examples/minimal) (a `local` store, no cloud account).

| Command | Synopsis | Changes the store | Runs providers |
| --- | --- | --- | --- |
| [`init`](#tegata-init) | `tegata init [--upgrade]` | no | no (installs them) |
| [`check`](#tegata-check) | `tegata check [<identity> <templates>]`, `tegata check --stale [--json]` | no | `describe` (none with `--stale`) |
| [`status`](#tegata-status) | `tegata status` | no | `describe` |
| [`apply`](#tegata-apply) | `tegata apply [--yes \| --dry-run]` | yes | `create`, `read` |
| [`rotate`](#tegata-rotate) | `tegata rotate [--yes \| --dry-run] <resource>...` | yes | `create` |
| [`settle`](#tegata-settle) | `tegata settle [--yes] <resource>...` | yes | no |
| [`prune`](#tegata-prune) | `tegata prune [--yes] [<resource>...]` | yes | `delete` |
| [`set`](#tegata-set) | `tegata set [--yes] <resource>` | yes | no |
| [`import`](#tegata-import) | `tegata import [--yes] <templates> <dir>` | yes | no |
| [`migrate`](#tegata-migrate) | `tegata migrate` | no | no |
| [`env`](#tegata-env) | `tegata env <name>` | no | no |
| [`render`](#tegata-render) | `tegata render [--project <p> \| --store-dir <d>] [--on-change <cmd>] [--exit-code] [--fail-on-missing] <templates> <out>` | no | no |

All commands except `render` run where you keep `tegata.yaml` and can administer the store. `render` runs on servers. Every command that changes a value (`apply`, `rotate`, `settle`, `set`, `import`) also writes the changed values to [destinations](configuration.md#destinations).

## Global options

| Option | Description |
| --- | --- |
| `-f, --file <FILE>` | The configuration file. Default `tegata.yaml`; also `TEGATA_FILE`. Not read by `render` |
| `-e, --env <NAME>` | The [environment](configuration.md#environments), when the configuration has `environments`; also `TEGATA_ENV`. Every command but `render`, `init`, `check` and `env` needs it then |
| `-h, --help` | Help for tegata or for one command (`tegata apply --help`) |
| `-V, --version` | The version |

Environment variables: see [the configuration reference](configuration.md#environment-variables).

## Exit status

| Status | Meaning |
| --- | --- |
| 0 | Success |
| 1 | An error, printed as `error: ...` on standard error. `check` also exits 1 when it finds problems, and `check --stale` when a resource is past due |
| 2 | A usage error, such as an unknown option |
| 3 | `render --exit-code`: a file changed |

`render` exits 0 even when some names stay unfilled; it lists them instead (`--fail-on-missing` makes that an error).

After an error, a `see:` line names the page that helps: the failed command's section of this reference, or the [configuration reference](configuration.md) when `tegata.yaml` cannot be read or is invalid. `tegata --help` lists the main pages, and `tegata <command> --help` ends with the link to that command's section.

```text
$ tegata settle admin_password
error: admin_password has no previous
see: https://github.com/k-kawa/tegata/blob/main/docs/reference/commands.md#tegata-settle
```

## Files

| File | Written by | Description |
| --- | --- | --- |
| `tegata.yaml` | you | The [configuration](configuration.md). Commit it |
| `tegata.lock` | `init` | The chosen provider versions, their requirement, signing key, and the SHA-256 of every platform's executable. Commit it |
| Environment files | you | An environment's overrides, when `environments` names a file. Commit them |
| Template directories | you | Files with `__NAME__` placeholders that `render` fills, usually one directory per identity |
| `$TEGATA_CACHE_DIR/...` | `init` | The installed providers |
| The output directory of `render` | `render` | Mode 0700; each file 0600, written to a temporary file and renamed |
| `store.local.dir` | every command | One plain-text JSON file per secret, mode 0600. For trying tegata and tests only |

## tegata init

```text
tegata init [--upgrade]
```

Installs every provider that has a `source`, and writes `tegata.lock`. For each provider it:

1. picks the newest release that matches `version`, or keeps the version in `tegata.lock`,
2. downloads `SHA256SUMS` and checks its minisign signature with `public_key` (the official key is built in for `github.com/k-kawa/tegata`),
3. downloads the executable for this machine and checks it against `SHA256SUMS`,
4. records the version and the SHA-256 of every platform's file in `tegata.lock`.

| Option | Description |
| --- | --- |
| `--upgrade` | Choose the newest versions the requirements allow, instead of the locked ones |

A release whose files changed after you locked it is refused. Other commands use only installed providers that match the lock, and tell you to run `tegata init` otherwise. With no `source` providers, it prints `no providers with a source`.

```sh
tegata init
GITHUB_TOKEN=$(gh auth token) tegata init --upgrade
TEGATA_PROVIDER_MIRROR=/mnt/mirror tegata init   # a machine without internet access
```

## tegata check

```text
tegata check [<identity> <templates>]
tegata check --stale [--json]
```

Checks the configuration and changes nothing: the YAML loads, and every provider answers `describe` and has the resource's `type`. Prints `ok`, or one problem per line and exits 1.

With an identity and a template directory (the one a server running as that identity gives to `tegata render`), it also checks that every `__NAME__` in the templates is made by some resource and readable by that identity. Secrets the identity can read but the templates do not use are listed as warnings, which do not fail the check. Run it once per identity, for example in CI next to the deploy configuration. The directory is relative to the current directory, as for `render`.

```text
$ tegata check edge templates/edge
warning: identity edge: can read MAIL, which its templates do not use
identity edge: KEY is in the templates but edge cannot read it (add edge to its readers)
identity edge: NOT_MADE is in the templates but no resource makes it
error: 2 problems
see: https://github.com/k-kawa/tegata/blob/main/docs/reference/commands.md#tegata-check
```

For a file in the [old form](configuration.md#the-old-form-and-tegata-migrate), `check` also compares every role's `templates`, since they decide what the role reads.

When the configuration has [environments](configuration.md#environments), `check` without `-e` checks every one of them (each line starts with the environment's name), and reports environments that use the same Google Cloud project or give access to the same IAM member. With `-e`, it checks that environment only.

```text
$ tegata check app templates/app
ok (staging, production)
```

### `--stale`

Checks the age of each resource's latest generation instead (see [Rotation age](configuration.md#rotation-age)). A resource older than its `max_age`, or past the expiry its provider reported, is a problem: it is printed with the command that rotates it, and `check` exits 1. A resource within `warn_before` of either is printed as a warning, which does not fail the check.

```text
$ tegata check --stale
warning: join_key: generation 2 is 3 days old, expires in 4 days; due soon: tegata rotate join_key
session_key: generation 4 is 97 days old (max 90d); rotate it now: tegata rotate session_key
error: 1 past due
see: https://github.com/k-kawa/tegata/blob/main/docs/guides/rotating-secrets.md#rotating-on-a-schedule
```

It reads only the store's metadata, never values, and runs no providers, so an identity with read-only access to the metadata is enough (on Secret Manager, `roles/secretmanager.viewer`). Without `-e`, a configuration with environments is checked in every environment.

| Option | Description |
| --- | --- |
| `--stale` | Check rotation age only |
| `--json` | With `--stale`: print the resources that are due or past due as JSON on standard output, for notifications. The exit status is the same |

```json
{
  "resources": [
    {
      "environment": "production",
      "resource": "session_key",
      "provider": "random",
      "generation": 4,
      "created": "2026-07-01T09:30:00Z",
      "age_seconds": 8380800,
      "max_age": "90d",
      "standing": "rotate_now",
      "command": "tegata rotate session_key"
    }
  ]
}
```

`standing` is `due`, `rotate_now` or `expired`. `environment`, `max_age` and `expires` are left out when there are none.

## tegata status

```text
tegata status
```

For each resource: its provider and type, the latest generation, how many older generations wait for `prune`, how old the latest generation is, its `max_age` and the provider's expiry if any, and a mark when a rotation is due (`DUE`, `ROTATE NOW`, `EXPIRED`; see [Rotation age](configuration.md#rotation-age)). For each secret: the latest enabled version and the identities that read it. Then the secrets labeled `managed-by=tegata` that are no longer in the YAML, then the state of each [destination](configuration.md#destinations).

```text
$ tegata status
admin_password (manual) generation 1, 3 days old
  ADMIN_PASSWORD                   v1           app
mail (value) -
  MAIL_FROM                        v1           app
session_key (random) generation 2 (+1 to prune), 97 days old (max 90d): ROTATE NOW
  SESSION_KEY                      v2           app
  PREVIOUS_SESSION_KEY             v1           app
```

`-` as the generation means none is recorded (a data resource, or no value yet). `(no value)` means the secret exists but has no enabled version.

## tegata apply

```text
tegata apply [--yes | --dry-run]
```

Makes the store match the YAML:

- creates the missing secrets, labeled `managed-by=tegata` and `tegata-resource=<resource>`;
- sets each secret's read access to exactly the members of its `readers`;
- moves the secrets of a renamed resource to the resource that now lists them;
- makes the first generation of every managed resource that has no value yet (`manual` resources wait for `tegata set`);
- reads data resources again, adding a version where an output changed and disabling the older ones;
- writes undelivered or changed values to destinations, and removes the deliveries the configuration no longer lists, for the secrets it declares in this environment ([removing a resource, and sharing a store](configuration.md#removing-a-resource-and-sharing-a-store)).

A failure in one resource does not stop the others; the command ends with `failed: <names>`.

| Option | Description |
| --- | --- |
| `--yes` | Do not ask: neither before making values that stop the previous one at once, nor in an environment with `confirm: true`. Required for those outside a terminal |
| `--dry-run` | Show which resources would get a new generation and what destinations would receive; change nothing |

```text
$ tegata apply
created secret SESSION_KEY
readers of SESSION_KEY: serviceAccount:app@example.iam.gserviceaccount.com
SESSION_KEY: version 1
session_key: generation 1
PREVIOUS_SESSION_KEY: version 1 (the value of SESSION_KEY)
admin_password: no value yet (put it with `tegata set admin_password`)
```

## tegata rotate

```text
tegata rotate [--yes | --dry-run] <resource>...
```

Makes a new generation of each named managed resource. If the resource has `previous`, the current value is copied there first, so services can accept both values for a while. Data resources cannot be rotated; `apply` reads them again. The options are the same as for `apply`.

```text
$ tegata rotate --dry-run session_key
would make a new generation of session_key
$ tegata rotate session_key
SESSION_KEY: version 2
session_key: generation 2
```

## tegata settle

```text
tegata settle [--yes] <resource>...
```

Copies the current value to `previous`, so the old value is no longer accepted. This ends a rotation of a value your application checks ([When a secret needs `previous`](../concepts/previous-and-settle.md)). Fails with `<resource> has no previous` when the resource has no `previous`.

```text
$ tegata settle session_key
PREVIOUS_SESSION_KEY: version 2 (the value of SESSION_KEY)
```

## tegata prune

```text
tegata prune [--yes] [<resource>...]
```

For each generation older than the latest (of every resource, when none is named): asks the provider to delete what it made, then disables that generation's versions and removes its record. Also disables the older versions of `previous`. Versions are disabled, never destroyed.

- Generations put by `set` or `import` for an external provider are not deleted on the provider, since tegata did not make them. `prune` says so, and you clean them up by hand.
- A provider can refuse (for example `tunnel_token`, while connectors started before the rotation are still connected). The command stops there.
- With nothing to prune, it prints nothing.

```text
$ tegata prune
SESSION_KEY: disabled version 1
session_key: pruned generation 1
PREVIOUS_SESSION_KEY: disabled version 1
```

## tegata set

```text
tegata set [--yes] <resource>
```

Puts a value by hand as a new generation: asked with hidden input on a terminal, otherwise read from standard input. Use it for `manual` resources, or to restore a value you kept. If the resource has `previous`, the current value is copied there first. Data resources are refused.

```sh
tegata set admin_password
op read op://infra/admin/password | tegata set admin_password
op read op://infra/admin/password | tegata -e production set --yes admin_password   # production has confirm: true
```

## tegata import

```text
tegata import [--yes] <templates> <dir>
```

For moving to tegata. `<dir>` holds files with the same names as the templates in `<templates>`, filled in by hand before tegata (for example the configuration files your servers use today). tegata matches each file against its template, takes the values out, and puts them into the secrets that have no value yet. Secrets that already have one are skipped (`already has a value; skipped`). `prune` never deletes imported generations on the provider.

```sh
tegata import templates/app /etc/app
```

`--yes` on `settle`, `prune`, `set` and `import` skips the question of an environment with `confirm: true` ([environments](configuration.md#environments)); elsewhere these commands do not ask.

## tegata env

```text
tegata env <name>
```

Prints the configuration of one [environment](configuration.md#environments): the shared part with that environment's overrides merged in, which is what the commands use with `-e <name>`. Changes nothing. Comments are not kept, and `confirm` is shown as a comment, so the output loads as a configuration of its own (for comparing it with another file, as in the [guide](../guides/staging-and-production.md#2-check-that-nothing-changed)).

```text
$ tegata env production
# confirm: true (commands that change the store ask first)
stores:
  main:
    gcp:
      location: asia-northeast1
      project: myapp-prod
...
```

## tegata migrate

```text
tegata migrate
```

Prints the configuration in the current form to standard output, and changes nothing. For a file in the [old form](configuration.md#the-old-form-and-tegata-migrate) (`store`, `roles`, `resources.*.secrets`), each secret's `readers` are filled in from the templates, which is what `apply` grants today, so `apply` after migrating changes nothing in the store. Comments are not kept. Other commands still read an old-form file, with a warning that names `tegata migrate`. A file with `environments` is in the current form already, and `migrate` says so.

```text
$ tegata migrate > tegata.new.yaml
# read access no longer comes from templates; to check that each identity can read its templates:
#   tegata check app ./templates/app
$ diff tegata.yaml tegata.new.yaml
$ mv tegata.new.yaml tegata.yaml
$ tegata check app ./templates/app
```

## tegata render

```text
tegata render [--project <project> | --store-dir <dir>] [--on-change <command>] [--exit-code]
              [--fail-on-missing] <templates> <out>
```

Fills every file directly in `<templates>` with the latest enabled version of each secret, and writes it to `<out>` under the same name. It reads only the store: no YAML, no providers, and no `tegata init` on the server.

A file whose content is the same as the one already in `<out>` is not written again, so its modification time stays. The others are written to a temporary file and renamed. So `render` can run on a timer, and says what changed.

| Option | Description |
| --- | --- |
| `--project <project>` | The Secret Manager project. Optional on Compute Engine VMs, where it comes from the metadata server |
| `--store-dir <dir>` | Read a `local` store instead (for trying tegata and tests) |
| `--on-change <command>` | Run the command with `sh -c` once, after writing, when any file changed. The changed files' names are in `TEGATA_CHANGED`, separated by spaces. If it fails, `render` fails (the files are written already) |
| `--exit-code` | Exit with 3 when a file changed, 0 when nothing did |
| `--fail-on-missing` | When a name is missing in the store or cannot be read, write nothing and fail, instead of leaving `__NAME__` in the file |

| Placeholder | Becomes |
| --- | --- |
| `__NAME__` | The value of secret `NAME` |
| `__NAME\|sha256__` | The SHA-256 of the value, as lowercase hex |

Names that are missing in the store, or that the caller cannot read, stay as written and are listed. Text between `__` and `__` that is not a valid name (`__init__`) is left alone. See [templates](configuration.md#templates).

```text
$ tegata render --project my-project templates/app /etc/app
filled 1 files
  not in the store: NEW_KEY

$ tegata render --on-change 'systemctl reload-or-restart app' templates/app /etc/app
filled 3 files
  changed: app.env
ran: systemctl reload-or-restart app (exit status: 0)
```

Running it on a timer, so that rotations reach the servers by themselves: [Running on servers](../guides/running-on-servers.md).
