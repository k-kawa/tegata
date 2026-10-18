# When a secret needs `previous`

Some secrets are rotated with `rotate` → `prune`. Others need `previous`, and `rotate` → wait → `settle` → `prune`. Which one depends on a single question: **who checks the value?**

- **Another service checks it** (a Cloudflare API token, a database password): the old and the new value can both be valid on that service for a while, and `prune` ends the old one. No `previous`.
- **Your application checks it** (the key that signs session cookies, a webhook secret, an encryption key): your application must accept the old value for a while, so it needs to know both. That is what `previous` is for, and `settle` ends the old value.

The rest of this page walks through both, with an example application.

## A key your application checks: signed session cookies

An application, "hako", signs the session cookie it gives each user:

```text
session = "user=tanaka" + signature(SESSION_KEY, "user=tanaka")
```

The signature proves hako made the cookie: if a user edits it to `user=admin`, the signature no longer matches and hako rejects it. Hako **makes** signatures when a user logs in, and **checks** them on every later request, with the same key. Today the key is `A`.

### Without `previous`: everyone is logged out

```text
10:00  the key becomes B; hako restarts
10:01  Tanaka, who logged in yesterday (cookie signed with A), opens a page
       hako checks with B → no match → "please log in again"
```

The moment the key changes, every session ends.

### With `previous`: nobody notices

Hako is written to make signatures with `SESSION_KEY`, and to accept a signature made with either `SESSION_KEY` or `PREVIOUS_SESSION_KEY`:

```yaml
resources:
  session_key:
    provider: random
    stores:
      main:
        - { name: SESSION_KEY, readers: [app] }
    previous: { name: PREVIOUS_SESSION_KEY }   # read by the readers of SESSION_KEY
```

```text
# templates/app/app.env
SESSION_KEY=__SESSION_KEY__
PREVIOUS_SESSION_KEY=__PREVIOUS_SESSION_KEY__
```

A rotation then goes like this:

| When | Command | `SESSION_KEY` | `PREVIOUS_SESSION_KEY` | Hako accepts |
| --- | --- | --- | --- | --- |
| Before | | A | A | A |
| 10:00 | `tegata rotate session_key`, then render | **B** | **A** | A and B |
| 10:01 | | B | A | Tanaka's A cookie passes; hako gives him a new cookie signed with B |
| 10:05 | | B | A | Suzuki logs in and gets a B cookie |
| 7 days later | `tegata settle session_key`, then render | B | **B** | B only |
| | `tegata prune session_key` | B | B | (version A is disabled in the store) |

- `rotate` copies the current value (A) into `previous` before making the new one.
- Between `rotate` and `settle`, cookies signed with A keep working. Wait as long as the longest-lived thing signed with A can live: here, the cookie's 7-day lifetime.
- `settle` copies the current value (B) into `previous`. Now both secrets hold B, and A stops being accepted.
- `prune` disables A's versions in the store.

Nobody is logged out, and after `settle`, a stolen A can no longer forge a cookie.

### What the application does

The application holds two keys, makes with the first, and checks with both:

```python
import hashlib, hmac, os

KEYS = [os.environ["SESSION_KEY"], os.environ["PREVIOUS_SESSION_KEY"]]

def sign(data: bytes) -> str:
    return hmac.new(KEYS[0].encode(), data, hashlib.sha256).hexdigest()

def verify(data: bytes, signature: str) -> bool:
    return any(
        hmac.compare_digest(hmac.new(k.encode(), data, hashlib.sha256).hexdigest(), signature)
        for k in KEYS
    )
```

Many frameworks do this already. Point their setting for older keys at `PREVIOUS_SESSION_KEY`:

| Framework | Current key | Older keys |
| --- | --- | --- |
| Django 4.1+ | `SECRET_KEY` | [`SECRET_KEY_FALLBACKS`](https://docs.djangoproject.com/en/stable/ref/settings/#secret-key-fallbacks) |
| Flask 3.1.1+ | `SECRET_KEY` | [`SECRET_KEY_FALLBACKS`](https://flask.palletsprojects.com/en/stable/config/#SECRET_KEY_FALLBACKS) (3.1.0 signed with the last fallback instead of the current key: [CVE-2025-47278](https://nvd.nist.gov/vuln/detail/CVE-2025-47278)) |
| Express, `cookie-session` | the first of [`keys`](https://github.com/expressjs/cookie-session#keys) | the rest of `keys` |

## A value another service checks: an API token

Hako calls Cloudflare's API with a token. Hako only **presents** the token; Cloudflare checks it.

```text
10:00  tegata rotate releases_read
       → tegata makes token B on Cloudflare; token A still works there
       render on the servers → hako restarts and uses B
10:05  every server uses B
       tegata prune releases_read → tegata deletes token A on Cloudflare
```

Cloudflare accepts A and B at the same time, so hako never needs to know both. No `previous`, no `settle`: the old token ends when `prune` deletes it.

A database password is similar, except that a database user has only one password: making a new one stops the old one at once. tegata asks before doing it, and the servers should render right after (see [values replaced at once](../guides/rotating-secrets.md#values-replaced-at-once-rotate-render-quickly-prune)). Still, the database checks the password, not your application, so there is no `previous`.

## An encryption key

An application that encrypts data at rest is also the one that decrypts it later, so it needs `previous` too, with one more step: data encrypted with the old key must be encrypted again with the new one before the old key goes.

```text
tegata rotate data_key        # DATA_KEY = new, PREVIOUS_DATA_KEY = old
# render; the application encrypts with the new key, and decrypts with either
# re-encrypt everything still encrypted with the old key (the application's own job)
tegata settle data_key        # only once nothing needs the old key
tegata prune data_key
```

Settling too early here does not log anyone out: it makes the data still encrypted with the old key unreadable. Wait for the re-encryption to finish.

## Webhooks

When another service signs requests to your application (a webhook secret), your application checks the signature, so it is the first case: keep `previous`, rotate, give the new secret to the sender, and `settle` once the sender uses it.

## Telling them apart

| What the application does with the value | Who checks it | `previous` | Steps |
| --- | --- | --- | --- |
| Signs or encrypts something, and checks or decrypts it later (session cookies, CSRF tokens, encrypted data) | The application | Yes | `rotate` → render → wait → `settle` → render → `prune` |
| Checks what someone else signed with it (webhook secrets) | The application | Yes | `rotate` → render → the sender switches → `settle` → render → `prune` |
| Presents it to another service (API tokens, database passwords, SMTP credentials) | The other service | No | `rotate` → render → `prune` |

The steps for each kind of value, with commands: [Rotating secrets](../guides/rotating-secrets.md). The key itself: [`previous`](../reference/configuration.md#resources).
