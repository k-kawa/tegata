# Contributing

Thanks for your interest in tegata.

## Before you start

- For anything larger than a small fix, please open an issue first, so we can agree on the approach.
- Security issues: see [SECURITY.md](SECURITY.md). Do not open a public issue.
- New providers do not have to live in this repository: the [provider protocol](docs/provider-protocol.md) lets you publish a provider from your own repository. Providers in this repository are the ones the maintainers can test and support.

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- Tests never call real cloud APIs. Providers and the store are tested against small fake servers in the tests.
- `unsafe` code is not allowed.
- Dependencies: prefer pure Rust, and permissive licenses only (MIT, Apache-2.0, BSD, and similar).
- Never print, log, or store secret values outside the secret store, including in error messages and in provider `state`.

## Language

Documentation, CLI help, error messages, commits, issues, and pull requests are in English. Some code comments are still in Japanese; translating them when you touch the code is welcome.

## License

tegata is licensed under the [Functional Source License, Version 1.1, ALv2 Future License](LICENSE.md) (FSL-1.1-ALv2). By contributing, you agree that your contributions are licensed under the same terms, including the grant of the Apache License 2.0 two years after each release.
