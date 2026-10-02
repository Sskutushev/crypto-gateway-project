# 4. Independence is counted by provider group, not by source

Date: 2026-10-02 (recorded)

## Status

Accepted

## Context

A verification policy that wants "two independent confirmations" has to say
what independent means. Two API keys for the same provider, two endpoints of
the same company, or a provider that resells another's node all look like two
sources and are one.

## Decision

Every chain source carries a `provider_group`. The verifier counts distinct
groups, never distinct sources: two sources in one group confirm each other
no more than one does. The group is the operator's statement that the
sources behind it share an operator, infrastructure or upstream.

Production policy requires at least two groups. The deployment definitions
ship one observer in the base and a second, with its own credential and its
own database login, in the production overlay, so the second group is a
deliberate configuration with its own secrets rather than a copy of a file.

## Consequences

- The gateway cannot verify that two groups are truly independent; that is an
  operational fact the operator asserts. `docs/owner-setup.md` says what to
  look for: a different company, different infrastructure, no shared
  upstream, separate credentials, and ideally one self-run node.
- A policy satisfied by one group in a non-production environment is a
  configuration the self-check names at start-up, not a silent downgrade.
