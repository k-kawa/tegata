# Minimal example

A configuration that runs on your machine without a cloud account: secrets are kept in `./store` (a `local` store: plain-text JSON files, for trying tegata only). It shows the three built-in providers and a rotation.

| Resource | Provider | Secrets |
| --- | --- | --- |
| `session_key` | `random`: 32 random bytes | `SESSION_KEY`, and `PREVIOUS_SESSION_KEY` for rotations |
| `admin_password` | `manual`: you type it | `ADMIN_PASSWORD` (the template uses only its SHA-256) |
| `mail` | `value`: a fixed, non-secret value | `MAIL_FROM` |

Run from this directory:

```sh
tegata check                    # every name in templates/app is made by a resource
tegata apply                    # creates the secrets and the random key
echo 'correct horse' | tegata set admin_password
tegata status

tegata render --store-dir store templates/app out
cat out/app.env
```

Rotate the key. In between, a service that accepts both `SESSION_KEY` and `PREVIOUS_SESSION_KEY` keeps working:

```sh
tegata rotate session_key       # new SESSION_KEY; the old value moves to PREVIOUS_SESSION_KEY
tegata render --store-dir store templates/app out
tegata settle session_key       # PREVIOUS_SESSION_KEY = SESSION_KEY
tegata prune session_key        # disable the old versions
tegata status
```

With Google Cloud Secret Manager, replace the store `main` with `gcp: { project: ..., location: ... }`, set the members of `app` to the service account of the server that renders, and run `tegata render templates/app out` on that server. See the [configuration reference](../../docs/reference/configuration.md).
