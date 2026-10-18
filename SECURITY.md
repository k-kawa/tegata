# Security policy

tegata handles secrets, so security reports are welcome and taken seriously.

## Reporting a vulnerability

Please **do not open a public issue**. Use GitHub's private vulnerability reporting: on this repository, go to **Security → Report a vulnerability**.

Include what you found, how to reproduce it, and what an attacker could do with it. You will get a reply within a week. Please give us a reasonable time to fix the issue before disclosing it.

## Supported versions

tegata is before 1.0. Fixes go into the latest release only.

## Scope

In scope: tegata itself, the providers in this repository, and the provider protocol (for example, a way to make tegata write a secret value to a log, to `state`, or to a file readable by others).

Out of scope: vulnerabilities in the cloud services and APIs that providers call, and providers maintained outside this repository (please report those to their authors).
