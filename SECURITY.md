# Security policy

tegata handles secrets, so security reports are welcome and taken seriously.

## Reporting a vulnerability

Please **do not open a public issue**. Use GitHub's private vulnerability reporting: on this repository, go to **Security → Report a vulnerability**.

Include what you found, how to reproduce it, and what an attacker could do with it. You will get a reply within a week. Please give us a reasonable time to fix the issue before disclosing it.

## Release signing key

The official provider releases are signed with this [minisign](https://jedisct1.github.io/minisign/) key (key ID `8DE281298F0646BE`). tegata has it built in, and `tegata init` refuses official releases that it did not sign.

```text
RWS+RgaPKYHijXCDQ/JCehzLyohjXERYxShqSb6lS71/MHaCS3agTxkB
```

To check a release by hand:

```sh
minisign -V -P RWS+RgaPKYHijXCDQ/JCehzLyohjXERYxShqSb6lS71/MHaCS3agTxkB -m SHA256SUMS
grep tegata-provider-cloudflare_0.1.0_linux_x86_64 SHA256SUMS | shasum -a 256 -c   # the file you downloaded
```

If this key ever has to change, the new key will be announced here and in a new tegata release.

## Supported versions

tegata is before 1.0. Fixes go into the latest release only.

## Scope

In scope: tegata itself, the providers in this repository, and the provider protocol (for example, a way to make tegata write a secret value to a log, to `state`, or to a file readable by others).

Out of scope: vulnerabilities in the cloud services and APIs that providers call, and providers maintained outside this repository (please report those to their authors).
