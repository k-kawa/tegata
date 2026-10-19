# How tegata compares with other tools

tegata does one job: the whole life of a credential that **another service** issues, such as a Cloudflare API token, a Tailscale auth key, or a database password. It creates the credential, stores it, gives read access to exactly the identities you list, rotates it, deletes what older generations made, fills it into templates on servers, and copies it to GitHub Actions secrets. All of it is declared in one YAML file, and nothing has to run between commands.

Many good tools cover part of this. This page says what each one does, so you can choose, and how tegata works alongside them. The facts were checked on 2026-10-20 against each tool's own documentation (linked); they change, so check the links for the current state.

## At a glance

| Tool | What you run | Stores values | Creates and rotates other services' credentials | Fills templates on servers |
| --- | --- | --- | --- | --- |
| **tegata** | A CLI (no server) | Google Cloud Secret Manager (the only store today) | Yes, through providers: Cloudflare, Tailscale, Amazon SES, Cloud SQL, and any provider you write | Yes (`tegata render`) |
| Google Cloud Secret Manager | A managed service | Yes | No. A rotation schedule only sends a Pub/Sub message; your own code makes the new value | No |
| AWS Secrets Manager | A managed service | Yes | For AWS services' managed secrets, and for listed partners (for example Datadog, GitLab, Twilio, Snowflake). Others need a Lambda function you write | No |
| OpenBao, HashiCorp Vault | A server you operate (storage, unsealing, high availability) | Yes | Dynamic, leased credentials from secrets engines (databases, clouds, and plugins) | With Vault Agent / OpenBao Agent |
| Infisical | A platform (cloud, or a server you operate) | Yes | Rotation for a listed set: databases, AWS IAM users, Azure, Okta and Auth0 client secrets, and more | Through its agent and CLI |
| External Secrets Operator | A Kubernetes controller | No (syncs from stores into Kubernetes Secrets) | Generators for some kinds: passwords, registry and GitHub access tokens, Vault dynamic secrets, and others | No (Kubernetes Secrets only) |
| Terraform | A CLI with state | In its state, unless you use write-only arguments | Can create tokens with providers (for example `cloudflare_api_token`); rotating means replacing the resource and handing the value on yourself | No |
| Teller, vals, SOPS | A CLI | No (Teller and vals read existing stores; SOPS encrypts files in Git) | No | Yes (Teller and vals fill values; SOPS decrypts) |

## When another tool fits better

- **You run Kubernetes, and only need secrets from a store inside the cluster:** External Secrets Operator.
- **You want short-lived credentials issued per request, with leases and revocation:** OpenBao or Vault. tegata makes long-lived values that you rotate on a schedule, and does not issue a credential per client.
- **Your store is AWS Secrets Manager, and your services are AWS services or its partners:** its managed rotation needs no code. tegata does not support AWS Secrets Manager as a store yet.
- **You want a web UI, audit logs, and access control for people, not just machines:** Infisical, or Vault and OpenBao.
- **You only need to fill files or environment variables from values that already exist:** vals or Teller are smaller.

## Where tegata fits

tegata is for small teams that already use a cloud secret store and want the credentials of their other services (Cloudflare, Tailscale, mail, databases) handled the same way as their own random keys, without running a server or Kubernetes:

- one YAML file says what exists, where it is stored, and who reads it; `tegata apply` makes the store match it
- `rotate` → (`settle`) → `prune` is the same three steps for every kind of credential, and `prune` deletes the old token at the service, not only the old value ([Rotating secrets](../guides/rotating-secrets.md))
- servers only need read access and `tegata render` ([Running on servers](../guides/running-on-servers.md))
- a provider is a separate executable that speaks JSON, so adding a service does not mean waiting for tegata ([Provider protocol](../reference/provider-protocol.md))

## Using them together

- **Admin credentials from another tool:** the tokens that let tegata make other tokens can be read from 1Password, Vault, AWS Secrets Manager, or any command, through [credential sources](../reference/configuration.md#credential_sources).
- **Values from Terraform:** the `terraform` provider reads Terraform outputs (for example a database host) into the store, next to the secrets ([providers](../reference/providers.md)).
- **CI:** copies of secrets go to GitHub Actions secrets, and are written again after each rotation ([destinations](../reference/configuration.md#destinations)).

## Sources (checked on 2026-10-20)

- Google Cloud Secret Manager, rotation schedules: https://docs.cloud.google.com/secret-manager/docs/secret-rotation
- AWS Secrets Manager, rotation: https://docs.aws.amazon.com/secretsmanager/latest/userguide/rotating-secrets.html; managed external secrets partners: https://docs.aws.amazon.com/secretsmanager/latest/userguide/mes-partners.html
- OpenBao, secrets engines: https://openbao.org/docs/secrets/; agent templates: https://openbao.org/docs/agent-and-proxy/agent/template/
- Infisical Agent: https://infisical.com/docs/integrations/platforms/infisical-agent
- Infisical, secret rotations: https://infisical.com/docs/integrations/secret-rotations
- External Secrets Operator, generators: https://external-secrets.io/latest/api/generator/
- Terraform write-only arguments: https://developer.hashicorp.com/terraform/language/resources/ephemeral/write-only
- Teller: https://github.com/tellerops/teller; vals: https://github.com/helmfile/vals; SOPS: https://github.com/getsops/sops
