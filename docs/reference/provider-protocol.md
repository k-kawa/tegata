# Provider protocol (version 1)

A provider is an executable that creates, reads, and deletes one kind of secret for tegata. tegata starts the executable once per operation, writes one JSON object to its standard input, and reads one JSON object from its standard output. Standard error is shown to the user as is, so a provider can print progress there (never values).

Providers can be written in any language. Rust providers can use the [`tegata-plugin`](../../crates/tegata-plugin) crate for the types and a small helper.

## Finding the executable

A provider is either installed by `tegata init` from a `source` (see [Installing providers](../../README.md#installing-providers) and [publishing providers](../guides/publishing-providers.md)), or a `command`:

```yaml
providers:
  cloudflare:
    command: tegata-provider-cloudflare   # relative to the YAML if it has a `/`; otherwise PATH, then next to tegata
    config: { account_id: 0123456789abcdef }
    credentials:
      api_token: { env: TEGATA_CLOUDFLARE_TOKEN, optional: true }
```

- The provider's `config` (account IDs and the like) is passed to every operation as `provider_config`. Each resource's own `config` is passed as `config`.
- `credentials` are passed as `credentials`. Each one is read from the environment variable `env`; if it is not set, tegata asks for it with hidden input on a terminal (the question is `prompt`, or `<provider> <name>`). With `optional: true`, a missing credential is left out instead of asked, and the provider decides what to do (for example, use a CLI login). Outside a terminal, a missing required credential is an error.

## Request

Every request has these fields:

| Field | Description |
| --- | --- |
| `protocol` | The protocol version. Always `1` for now |
| `op` | `describe`, `create`, `read`, or `delete` |
| `type` | The resource type (`api_token`, ...). Empty for `describe` |
| `config` | The resource's `config` |
| `provider_config` | The provider's `config` |
| `credentials` | A map of credential name → value. Never log these |
| `base_dir` | The directory of the YAML file (for relative paths in `config`) |
| `resource` | The resource's name (for messages) |

## Operations

### `describe`

Tells tegata which types the provider supports.

Response:

```json
{ "types": { "api_token": "managed", "tunnel_token": "managed" }, "replaces": ["tunnel_token"] }
```

- `types`: each type is `managed` (tegata creates and deletes it) or `data` (tegata only reads it)
- `replaces` (optional): types that can hold only one value, so creating a new one stops the previous value from working at once. tegata asks the user before creating these. Leave it out if there are none

### `create` (managed types)

Creates a new value. Called by `apply` for resources that have no value yet, and by `rotate`.

Response:

```json
{ "outputs": { "id": "…", "value": "…" }, "state": { "token_id": "…" }, "expires": "2026-10-25T09:30:00Z" }
```

- `outputs`: output name → value. The resource's `secrets` map secret names to these output names
- `state`: anything the provider needs later to delete this value (an ID, a creation time). **Never put secret values here**: tegata stores `state` in plain text in the secret's annotations
- `expires` (optional): when the new value stops working on the service's side, in RFC 3339. tegata records it with the generation; `tegata status` shows it, and `tegata check --stale` fails once it has passed. Leave it out for values that do not expire. In Rust, set `Created::expires`

### `read` (data types)

Reads current values. Called by `apply` every time.

Response: `{ "outputs": { … } }`

### `delete` (managed types)

Deletes what an older generation created. Called by `prune`. The request also has:

| Field | Description |
| --- | --- |
| `state` | The `state` that `create` returned for the generation being deleted |
| `current_state` | The `state` of the latest generation, which is kept (optional; left out when there is none). Useful for single-value types: for example, to check that nothing still uses the old value before cleaning up |

Response: `{}`

A provider may refuse to delete (for example, when the old value is still in use) by returning an error; tegata then stops and shows the message.

## Errors

Exit with a non-zero status and write `{"error": "message"}` to standard output. The message is shown to the user. Do not include secret values in it.

## Compatibility

Fields may be added to requests and responses without changing the protocol version. Providers must ignore request fields they do not know, and tegata ignores response fields it does not know. Changes that would make an old provider or an old tegata misread a message raise the protocol version.
