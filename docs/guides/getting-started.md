# Getting started

Try tegata on your own machine in about ten minutes, without a cloud account: make secrets, fill a template with them, and rotate one. Then move the same setup to Google Cloud Secret Manager.

You need the `tegata` executable ([Install](../../README.md#install)). The example uses only the built-in providers, so `tegata init` is not needed.

## 1. The configuration

Copy [examples/minimal](../../examples/minimal) and work in it:

```sh
cp -r examples/minimal /tmp/tegata-try && cd /tmp/tegata-try
```

`tegata.yaml` declares where secrets live, who reads them, and what to make. Each secret lists its `readers`; `previous` is read by the readers of the value it copies:

```yaml
stores:
  main:
    local:
      dir: ./store                    # plain-text JSON files: for trying tegata only

identities:
  app:
    members: [serviceAccount:app@example.iam.gserviceaccount.com]
    templates: ./templates/app        # check compares them with what app can read

resources:
  session_key:
    provider: random                  # 32 random bytes
    config: { bytes: 32 }
    stores:
      main:
        - { name: SESSION_KEY, readers: [app] }
    previous: { name: PREVIOUS_SESSION_KEY }
  admin_password:
    provider: manual                  # you type it
    stores:
      main:
        - { name: ADMIN_PASSWORD, readers: [app] }
  mail:
    provider: value                   # a fixed, non-secret value
    config: { from: "Example <noreply@example.com>" }
    stores:
      main:
        - { output: from, name: MAIL_FROM, readers: [app] }
```

`templates/app/app.env` is the file a server running as `app` fills in. `tegata check` makes sure `app` can read every name in it:

```text
SESSION_KEY=__SESSION_KEY__
PREVIOUS_SESSION_KEY=__PREVIOUS_SESSION_KEY__
ADMIN_PASSWORD_SHA256=__ADMIN_PASSWORD|sha256__
MAIL_FROM=__MAIL_FROM__
```

## 2. Check and apply

```text
$ tegata check
ok
$ tegata apply
created secret ADMIN_PASSWORD
readers of ADMIN_PASSWORD: serviceAccount:app@example.iam.gserviceaccount.com
admin_password: no value yet (put it with `tegata set admin_password`)
created secret MAIL_FROM
readers of MAIL_FROM: serviceAccount:app@example.iam.gserviceaccount.com
MAIL_FROM: version 1 (from mail)
created secret SESSION_KEY
readers of SESSION_KEY: serviceAccount:app@example.iam.gserviceaccount.com
created secret PREVIOUS_SESSION_KEY
readers of PREVIOUS_SESSION_KEY: serviceAccount:app@example.iam.gserviceaccount.com
SESSION_KEY: version 1
session_key: generation 1
PREVIOUS_SESSION_KEY: version 1 (the value of SESSION_KEY)
```

`apply` made the random key, and gave the members of `app` read access to the four secrets. The `manual` password waits for you:

```text
$ echo 'correct horse' | tegata set admin_password
ADMIN_PASSWORD: version 1
admin_password: generation 1
$ tegata status
admin_password (manual) generation 1
  ADMIN_PASSWORD                   v1           app
mail (value) -
  MAIL_FROM                        v1           app
session_key (random) generation 1
  SESSION_KEY                      v1           app
  PREVIOUS_SESSION_KEY             v1           app
```

## 3. Render

On a server, `render` fills the templates from the store. It needs no YAML and no providers:

```text
$ tegata render --store-dir store templates/app out
filled 1 files
$ cat out/app.env
SESSION_KEY=3f9c…
PREVIOUS_SESSION_KEY=3f9c…
ADMIN_PASSWORD_SHA256=9c0f…
MAIL_FROM=Example <noreply@example.com>
```

`out` is created with mode 0700 and `app.env` with 0600.

## 4. Rotate

```text
$ tegata rotate session_key
SESSION_KEY: version 2
session_key: generation 2
$ tegata render --store-dir store templates/app out
filled 1 files
$ tegata settle session_key
PREVIOUS_SESSION_KEY: version 2 (the value of SESSION_KEY)
$ tegata prune session_key
SESSION_KEY: disabled version 1
session_key: pruned generation 1
PREVIOUS_SESSION_KEY: disabled version 1
```

Between `rotate` and `settle`, a service that accepts both `SESSION_KEY` and `PREVIOUS_SESSION_KEY` keeps working with values made before the rotation. [Rotating secrets](rotating-secrets.md) has the steps for every kind of value.

## 5. Move to Secret Manager

1. Replace the store, and set the members of `app` to the service account of the server that renders:

    ```yaml
    stores:
      main:
        gcp:
          project: my-project
          location: us-central1
    identities:
      app:
        members: [serviceAccount:app-vm@my-project.iam.gserviceaccount.com]
        templates: ./templates/app
    ```

2. Run `tegata apply` with an account that can administer Secret Manager in the project (your `gcloud` login is used).
3. On the server: `tegata render templates/app /etc/app`. On a Compute Engine VM, the project and the credentials come from the VM; elsewhere add `--project my-project` and give the server's identity a way to authenticate.

## Next

- Make tokens for other services with [providers](../reference/providers.md) (`tegata init` installs them).
- Copy secrets to GitHub Actions with [destinations](../reference/configuration.md#destinations).
- Every key and command: the [configuration reference](../reference/configuration.md) and the [command reference](../reference/commands.md).
