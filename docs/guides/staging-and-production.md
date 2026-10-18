# Staging and production

Run staging and production from one configuration: the same resources and readers, in separate projects, read by separate service accounts. Each environment's file holds only what differs, so a resource added for staging cannot be forgotten in production, and production's differences are one file to review.

Every key is described in the [configuration reference](../reference/configuration.md#environments).

## 1. Split the configuration

Start from the file you have (in the current form; [migrate](../reference/commands.md#tegata-migrate) an old one first). Keep what both environments share in `tegata.yaml`, and move what differs to a file per environment:

```text
tegata.yaml           stores (without the project), identities (with []), providers, resources, environments
envs/staging.yaml     the project, the members, provider and resource settings that differ
envs/production.yaml  the same for production, and confirm: true
tegata.lock           shared: every environment uses the same providers
```

```yaml
# tegata.yaml
stores:
  main:
    gcp: { location: asia-northeast1 }
identities:
  app: []
environments:
  staging: envs/staging.yaml
  production: envs/production.yaml
resources:
  session_key:
    provider: random
    stores:
      main:
        - { name: SESSION_KEY, readers: [app] }
```

```yaml
# envs/production.yaml
confirm: true
stores: { main: { gcp: { project: myapp-prod } } }
identities: { app: [serviceAccount:app@myapp-prod.iam.gserviceaccount.com] }
```

Things that usually differ: the project, the service accounts, names on other services (a Cloudflare token's name, a Cloud SQL instance, a tunnel), a Terraform directory (paths stay relative to `tegata.yaml`), and the destinations. Things that should not differ, and cannot: which resources exist, and who reads what.

## 2. Check that nothing changed

Before applying, compare each environment with the file it replaces. `tegata env` prints an environment's merged configuration:

```sh
tegata env staging > /tmp/staging.yaml
tegata -f staging.old.yaml migrate > /tmp/staging.old.yaml   # the old file, in the same normalized form
tegata -f /tmp/staging.yaml migrate | diff /tmp/staging.old.yaml -
tegata check                          # every environment, and that they share no project or member
tegata -e staging apply --dry-run     # nothing to make
```

The first `apply` in each environment labels its secrets with `tegata-env=<name>`; nothing else changes.

## 3. Work in one environment at a time

```sh
tegata -e staging rotate session_key
export TEGATA_ENV=staging             # or for a whole shell
tegata status
```

Production asks before changing anything, because of `confirm: true`:

```text
$ tegata -e production rotate session_key
production: make a new generation of session_key
Type the environment name to continue: production
```

In CI, pass `--yes` and protect the job instead, for example with a GitHub environment that needs a reviewer.

## 4. Keep production's credentials out of staging's shell

A provider's credentials can come from a different variable in each environment:

```yaml
# envs/production.yaml
providers:
  cloudflare:
    credentials:
      api_token: { env: TEGATA_CLOUDFLARE_TOKEN_PROD }
```

Then a token exported for staging is never used against production by accident.
