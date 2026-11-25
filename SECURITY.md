# Security Policy

## Supported versions

Only the latest released version receives security fixes. This is a
single-maintainer project; older releases are not backpatched.

| Version | Supported |
| ------- | --------- |
| 0.1.x   | yes       |
| < 0.1   | no        |

## Reporting a vulnerability

**Please do not open a public issue for a security problem.**

Email **ronimuliawan12@gmail.com** with:

- the affected version (`ghostdu --version`),
- what an attacker gains, and the conditions required,
- reproduction steps or a proof of concept,
- any suggested fix.

Private vulnerability reporting through GitHub is not enabled on this
repository, so email is the intended channel. You can expect an
acknowledgement, an assessment, and a coordinated disclosure timeline. Credit
is given in the release notes unless you prefer otherwise.

## Scope

ghostdu deletes and moves real files, and queries the Docker daemon. Reports
that matter most:

- **Deletion guardrails** — any path that removes or trashes an object the
  safety tier should have protected, or that bypasses the identity
  verification between confirmation and execution.
- **Docker socket handling** — anything that lets a hostile or wedged daemon
  escalate beyond reporting/pruning, exceed the response or time bounds, or
  influence which resources are pruned.
- **Terminal output** — control characters or escape sequences from
  filesystem or daemon data reaching the terminal unsanitized.
- **Privilege** — any path that requires or acquires privileges beyond the
  invoking user's existing filesystem and Docker socket permissions.

## Security model

The threat model, the guarantees the project makes, and the guarantees it
explicitly does **not** make are documented in the README:

- "Safety model" and identity-verification notes — deletion guardrails, the
  bound on open handles, and the concurrent-writer boundary.
- "Docker authority" — direct `/var/run/docker.sock` use, no privilege
  elevation, no CLI context switching.
- Trash/recovery notes — orphan metadata handling and what is not swept
  automatically.

Read those before reporting a behavior that is documented as an accepted
limitation; it may already be intentional.

## Out of scope

- Issues requiring an already-compromised account or root on the host.
- Denial of service against your own daemon by acting as root.
- Reports about unsupported older versions.
