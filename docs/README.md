# tegata documentation

## Guides

Step by step, for a task.

- [Getting started](guides/getting-started.md): make, render, and rotate secrets on your machine, then move to Secret Manager
- [Rotating secrets](guides/rotating-secrets.md): the steps for each kind of value, and what to do when something goes wrong
- [Publishing providers](guides/publishing-providers.md): release your own provider for `tegata init`, and how the official releases are made

## Reference

Everything, exactly.

- [Commands](reference/commands.md): every command, its options, output, and exit status
- [Configuration](reference/configuration.md): every key of `tegata.yaml`, templates, generations, `tegata.lock`, environment variables
- [Providers](reference/providers.md): the providers in this repository, their settings, outputs, and rotation behavior
- [Provider protocol](reference/provider-protocol.md): what a provider reads and writes, for writing your own

See also: [Security policy](../SECURITY.md) (reporting vulnerabilities, the release signing key) and [Contributing](../CONTRIBUTING.md).
