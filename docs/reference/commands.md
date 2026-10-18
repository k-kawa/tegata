# Command reference

Every command of `tegata`, its options, and what it prints. The outputs below were captured on [examples/minimal](../../examples/minimal) (a `local` store, no cloud account).

| Command | Synopsis | Changes the store | Runs providers |
| --- | --- | --- | --- |
| [`init`](#tegata-init) | `tegata init [--upgrade]` | no | no (installs them) |
| [`check`](#tegata-check) | `tegata check` | no | `describe` |
| [`status`](#tegata-status) | `tegata status` | no | `describe` |
| [`apply`](#tegata-apply) | `tegata apply [--yes \| --dry-run]` | yes | `create`, `read` |
| [`rotate`](#tegata-rotate) | `tegata rotate [--yes \| --dry-run] <resource>...` | yes | `create` |
| [`settle`](#tegata-settle) | `tegata settle <resource>...` | yes | no |
| [`prune`](#tegata-prune) | `tegata prune [<resource>...]` | yes | `delete` |
| [`set`](#tegata-set) | `tegata set <resource>` | yes | no |
| [`import`](#tegata-import) | `tegata import <role> <dir>` | yes | no |
| [`render`](#tegata-render) | `tegata render [--project <p> \| --store-dir <d>] <templates> <out>` | no | no |

All commands except `render` run where you keep `tegata.yaml` and can administer the store. `render` runs on servers. Every command that changes a value (`apply`, `rotate`, `settle`, `set`, `import`) also writes the changed values to [destinations](configuration.md#destinations).

## Global options

| Option | Description |
| --- | --- |
| `-f, --file <FILE>` | The configuration file. Default `tegata.yaml`; also `TEGATA_FILE`. Not read by `render` |
| `-h, --help` | Help for tegata or for one command (`tegata apply --help`) |
| `-V, --version` | The version |

Environment variables: see [the configuration reference](configuration.md#environment-variables).

## Exit status

| Status | Meaning |
| --- | --- |
| 0 | Success |
| 1 | An error, printed as `error: ...` on standard error. `check` also exits 1 when it finds problems |
| 2 | A usage error, such as an unknown option |

`render` exits 0 even when some names stay unfilled; it lists them instead.

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
| Template directories | you | Files with `__NAME__` placeholders, one directory per role |
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
tegata check
```

Checks the configuration and changes nothing: the YAML loads, every provider answers `describe` and has the resource's `type`, and every `__NAME__` in a role's templates is made by some resource. Prints `ok`, or one problem per line and exits 1.

```text
$ tegata check
role app: NOT_MADE is in the templates but no resource makes it
error: 1 problems
```

## tegata status

```text
tegata status
```

For each resource: its provider and type, the latest generation, and how many older generations wait for `prune`. For each secret: the latest enabled version and the roles that read it. Then the secrets labeled `managed-by=tegata` that are no longer in the YAML, then the state of each [destination](configuration.md#destinations).

```text
$ tegata status
admin_password (manual) generation 1
  ADMIN_PASSWORD                   v1           app
mail (value) -
  MAIL_FROM                        v1           app
session_key (random) generation 2 (+1 to prune)
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
- sets each secret's readers to exactly the roles whose templates use it;
- moves the secrets of a renamed resource to the resource that now lists them;
- makes the first generation of every managed resource that has no value yet (`manual` resources wait for `tegata set`);
- reads data resources again, adding a version where an output changed and disabling the older ones;
- writes undelivered or changed values to destinations, and removes the secrets tegata delivered that are no longer configured.

A failure in one resource does not stop the others; the command ends with `failed: <names>`.

| Option | Description |
| --- | --- |
| `--yes` | Do not ask before making values that stop the previous one at once. Required for those outside a terminal |
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
tegata settle <resource>...
```

Copies the current value to `previous`, so the old value is no longer accepted. This ends a rotation. Fails with `<resource> has no previous` when the resource has no `previous`.

```text
$ tegata settle session_key
PREVIOUS_SESSION_KEY: version 2 (the value of SESSION_KEY)
```

## tegata prune

```text
tegata prune [<resource>...]
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
tegata set <resource>
```

Puts a value by hand as a new generation: asked with hidden input on a terminal, otherwise read from standard input. Use it for `manual` resources, or to restore a value you kept. If the resource has `previous`, the current value is copied there first. Data resources are refused.

```sh
tegata set admin_password
op read op://infra/admin/password | tegata set admin_password
```

## tegata import

```text
tegata import <role> <dir>
```

For moving to tegata. `<dir>` holds files with the same names as the role's templates, filled in by hand before tegata (for example the configuration files your servers use today). tegata matches each file against its template, takes the values out, and puts them into the secrets that have no value yet. Secrets that already have one are skipped (`already has a value; skipped`). `prune` never deletes imported generations on the provider.

```sh
tegata import app /etc/app
```

## tegata render

```text
tegata render [--project <project> | --store-dir <dir>] <templates> <out>
```

Fills every file directly in `<templates>` with the latest enabled version of each secret, and writes it to `<out>` under the same name. It reads only the store: no YAML, no providers, and no `tegata init` on the server.

| Option | Description |
| --- | --- |
| `--project <project>` | The Secret Manager project. Optional on Compute Engine VMs, where it comes from the metadata server |
| `--store-dir <dir>` | Read a `local` store instead (for trying tegata and tests) |

| Placeholder | Becomes |
| --- | --- |
| `__NAME__` | The value of secret `NAME` |
| `__NAME\|sha256__` | The SHA-256 of the value, as lowercase hex |

Names that are missing in the store, or that the caller cannot read, stay as written and are listed. Text between `__` and `__` that is not a valid name (`__init__`) is left alone. See [templates](configuration.md#templates).

```text
$ tegata render --project my-project templates/app /etc/app
filled 1 files
  not in the store: NEW_KEY
```
