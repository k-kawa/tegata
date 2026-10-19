# Admin credentials and CI

Where to keep the credentials that let providers make other credentials, and how to run tegata in CI without long-lived keys.

Providers need an **admin credential** to make what you use: the Cloudflare token that can create account API tokens, the Tailscale OAuth client that can create auth keys. These are the most powerful secrets in a tegata setup. Keep them:

- **out of the store tegata manages.** Whoever runs `tegata apply` administers that project and can grant themselves access to anything in it.
- **in one place, not one per environment,** readable only by the operators and the CI runner.
- **away from long-lived keys, where a provider allows it.**

Every key used here is described in the [configuration reference](../reference/configuration.md#credential_sources).

## 1. Keep no key where you do not need one

| Provider | Admin credential | Without a stored key |
| --- | --- | --- |
| `gcp`, and the store | Google credentials | Yes: `gcloud` on a laptop, Workload Identity Federation in CI, the service account on a VM |
| `aws` | AWS credentials | Yes: the AWS SDK's default chain, so `aws sso login` on a laptop and a role assumed with GitHub's OIDC token in CI |
| `cloudflare` | An API token that can create tokens | No. Without one, a laptop can use the `cf` login (`optional: true`); CI needs the token |
| `tailscale` | An OAuth client | No |

Store only the last two, scoped as narrowly as the provider allows: for Cloudflare, a token with only "API Tokens Write", optionally limited to known IP addresses; a separate one for production.

## 2. Put them in an admin project

Make a Google Cloud project for admin credentials only (here `myorg-admin`). Nothing runs there, and only two kinds of accounts can read its secrets: the operators and the CI runner (`tegata-runner@myorg-admin.iam.gserviceaccount.com`). Turn on the data access audit logs for Secret Manager in it, so every read is recorded.

Then point the providers at it:

```yaml
# tegata.yaml
credential_sources:
  admin:
    gcp: { project: myorg-admin }
providers:
  cloudflare:
    source: github.com/k-kawa/tegata
    version: "^0.4"
    config: { account_id: 0123456789abcdef }
    credentials:
      api_token:
        from: { source: admin, secret: cloudflare-admin-token }
        env: TEGATA_CLOUDFLARE_TOKEN    # a one-off override still works
```

With [environments](staging-and-production.md), production can read its own token:

```yaml
# envs/production.yaml
providers:
  cloudflare:
    credentials:
      api_token: { from: { source: admin, secret: cloudflare-admin-token-prod } }
```

A password manager works the same way through a `command` source, for example 1Password:

```yaml
credential_sources:
  op:
    command: [op, read]
providers:
  cloudflare:
    credentials:
      api_token: { from: { source: op, ref: "op://infra/cloudflare-admin/token" } }
```

## 3. Manage the admin credentials with tegata

The admin credentials are secrets too: give them versions, readers, and a place to see how old they are. A small configuration of their own does it, with the admin project as its store and `manual` resources, so it needs no admin credential itself:

```yaml
# admin.yaml
stores:
  main:
    gcp: { project: myorg-admin, location: asia-northeast1 }
identities:
  runner: [serviceAccount:tegata-runner@myorg-admin.iam.gserviceaccount.com]
  operators: [group:operators@example.com]
resources:
  cloudflare_admin:
    provider: manual
    stores:
      main:
        - { name: cloudflare-admin-token, readers: [runner, operators] }
  cloudflare_admin_prod:
    provider: manual
    stores:
      main:
        - { name: cloudflare-admin-token-prod, readers: [runner, operators] }
```

```sh
tegata -f admin.yaml apply                  # the secrets and their readers
op read op://infra/cloudflare-admin/token | tegata -f admin.yaml set cloudflare_admin
tegata -f admin.yaml status                 # versions, and who reads them
```

To replace an admin token: make a new one on the provider, `tegata -f admin.yaml set` it, check that the main configuration still works (`tegata -e staging rotate` something harmless, or `apply --dry-run`), then revoke the old one on the provider and `tegata -f admin.yaml prune`.

## 4. Run tegata in CI without keys

In GitHub Actions, the workflow becomes the runner's service account through Workload Identity Federation. It reads the admin credentials from the admin project and administers the environment's project; no key is stored in GitHub.

```yaml
# .github/workflows/rotate.yml
on: workflow_dispatch
jobs:
  rotate:
    runs-on: ubuntu-latest
    environment: production                 # a GitHub environment with required reviewers
    permissions: { id-token: write, contents: read }
    steps:
      - uses: actions/checkout@v5
      - uses: google-github-actions/auth@v3
        with:
          workload_identity_provider: projects/123456/locations/global/workloadIdentityPools/github/providers/github
          service_account: tegata-runner@myorg-admin.iam.gserviceaccount.com
      - uses: google-github-actions/setup-gcloud@v3
      - run: |
          # install tegata (checked as in the README), then:
          tegata -e production init
          tegata -e production rotate --yes session_key
```

The runner needs:

| Where | Role |
| --- | --- |
| The admin project | `roles/secretmanager.secretAccessor` on the admin secrets it uses |
| Each environment's project | What `apply`, `rotate` and `prune` need: for example `roles/secretmanager.admin` |
| AWS (for the `aws` provider) | A role it assumes with GitHub's OIDC token, allowed to manage the SES user's access keys |

`--yes` answers the question of an environment with `confirm: true`; the GitHub environment's reviewers take its place.
