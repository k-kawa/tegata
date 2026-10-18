# Rotating secrets

How to replace a secret with a new value without breaking the services that use it. The steps depend on whether the old and the new value can work at the same time.

| The value | Examples | Steps |
| --- | --- | --- |
| Checked by your own service, which can accept two values | Session keys, signing keys, webhook secrets (`random` with `previous`) | `rotate` → render → `settle` → render → `prune` |
| Issued by another service, old and new both work until the old one is deleted | `cloudflare/api_token`, `tailscale/auth_key`, `aws/ses_smtp_credentials` | `rotate` → render → `prune` |
| Only one value exists; a new one stops the old one at once | `gcp/sql_user`, `cloudflare/tunnel_token` | `rotate` (asks first) → render at once → `prune` |
| Typed in by hand | `manual` | `set` → render → `prune` |

"Render" means: run `tegata render` on every server that uses the secret and restart (or reload) the services that read the files. GitHub Actions secrets ([destinations](../reference/configuration.md#destinations)) need nothing: tegata writes the new value during `rotate`, `settle` and `set`, and workflows use it from their next run.

`tegata status` shows where each resource is: `generation 2 (+1 to prune)` means a rotation has not been cleaned up yet.

## Keys your service checks: rotate, settle, prune

Give the resource a `previous` secret, and make the service accept a value that matches either one:

```yaml
resources:
  session_key:
    provider: random
    secrets: { SESSION_KEY: value }
    previous: PREVIOUS_SESSION_KEY
```

```text
# templates/app/app.env
SESSION_KEY=__SESSION_KEY__
PREVIOUS_SESSION_KEY=__PREVIOUS_SESSION_KEY__
```

The service signs with `SESSION_KEY` and accepts both.

```sh
tegata rotate session_key   # SESSION_KEY: new; PREVIOUS_SESSION_KEY: the old value
# render and restart: services sign with the new key, and still accept the old one
tegata settle session_key   # PREVIOUS_SESSION_KEY: the new value too
# render and restart: the old value is no longer accepted
tegata prune session_key    # disable the old versions in the store
```

Wait between `rotate` and `settle` as long as values signed with the old key need to stay valid (for session keys, the session lifetime).

## Tokens that live side by side: rotate, prune

The provider makes a second token next to the first one. Both work until `prune` deletes the old one.

```sh
tegata rotate releases_read # a second token exists; the old one still works
# render and restart on every server that uses it
tegata prune releases_read  # delete the old token on the provider, disable the old versions
```

Run `prune` only after every server uses the new value: services still on the old token stop working when it is deleted. For `aws/ses_smtp_credentials`, an IAM user can hold at most two access keys, so prune before you rotate again.

## Values replaced at once: rotate, render quickly, prune

A Cloud SQL user has one password, and a Cloudflare Tunnel has one secret. Making a new one stops the old one immediately: open connections stay, new ones fail until the servers have the new value. `apply` and `rotate` show what will break and who reads it, and ask you to type `yes` (`--yes` skips the question in scripts; `--dry-run` only shows).

```sh
tegata rotate --dry-run db_password   # see what would change
tegata rotate db_password             # type yes
# render and restart at once on every server that uses it
tegata prune db_password              # disable the old versions
```

For `cloudflare/tunnel_token`, `prune` checks that no connector started before the rotation is still connected, and refuses (naming the connectors) until every `cloudflared` has been restarted with the new token. For `gcp/sql_user`, `prune` deletes nothing on Cloud SQL; the old password already stopped working.

## Values typed by hand: set

```sh
tegata set admin_password   # hidden input; or pipe it on standard input
# render and restart
tegata prune admin_password
```

If the resource has `previous`, `set` copies the current value there first, as `rotate` does, and the steps of the first section apply.

## If something goes wrong

- **A server cannot read the new value.** `render` leaves `__NAME__` in the file and lists the name. Check that the role's templates use the name and that `tegata apply` ran after you added it (`tegata status` shows the readers).
- **You pruned before a server had the new value.** The new value is already in the store: render and restart on that server. Old versions are disabled, not destroyed, so you can still enable one in Secret Manager to read it, but a token deleted on the provider cannot be brought back.
- **The old value leaked.** Rotate, render, and prune at once, without waiting between the steps.
