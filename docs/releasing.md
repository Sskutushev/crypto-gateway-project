# Releasing and upgrading

How versions are numbered, which image runs against which schema, how an
operator upgrades and rolls back, and how a release is cut. No release has
been tagged yet; the first one will be `v0.1.0`.

## Versioning

The project follows [Semantic Versioning](https://semver.org/). A tag
`vMAJOR.MINOR.PATCH` publishes the image
`ghcr.io/sskutushev/crypto-gateway-project:MAJOR.MINOR.PATCH` (no `v`), and
the tag must equal the `[workspace.package] version` in `Cargo.toml`.

Three things carry a version, and a release note says which of them moved:

| Surface | Where its version lives | Breaking change |
|---|---|---|
| HTTP API and webhooks | the `/v1` path prefix, `info.version` in [`openapi.json`](openapi.json), the `v1` signature scheme | removing or renaming a route, field, status or event type; changing what an existing field means; a new required request field |
| Database schema | the highest applied migration in `_sqlx_migrations` | a migration the previous image cannot run against (a dropped or renamed column or table, a new `NOT NULL` column the previous image does not write) |
| Configuration | the env files in `deploy/env/` and `deploy/k8s/configmap.yaml` | a new required variable, or a changed meaning of an existing one |

Before `1.0.0`, a MINOR release may break any of the three and says so in its
notes; a PATCH release never does. From `1.0.0` a break needs a MAJOR
release.

## Compatibility matrix

Every release adds its row in the same pull request that moves its
`CHANGELOG.md` section out of `Unreleased`.

| Image version | Migrations | HTTP API | Webhook signature | Upgrades from |
|---|---|---|---|---|
| unreleased (`main`) | `0001`–`0017` | `/v1`, OpenAPI `0.1.0` | `v1` (HMAC-SHA256; several `v1` values during a rotation) | — |

How an image and a schema meet:

- An image carries every migration up to its own. Run in migrate-only mode
  against an older schema, it applies the missing ones in order.
- An image refuses to migrate a database that has a migration it does not
  know (`migration N was previously applied but is missing`). That is how an
  older image run by mistake with migrations enabled fails, instead of
  pretending the schema is its own.
- Serving processes run with `GATEWAY_RUN_MIGRATIONS=false` in production and
  do not check migrations at all. Whether an older image may serve against a
  newer schema is a property of the migrations in between, and each release
  note states it (see the rollback rules below).
- An applied migration is never edited, not even its line endings: the
  database stores a SHA-384 of the file's bytes, and every later image refuses
  a database whose checksum differs (`migration N was previously applied but
  has been modified`). `.gitattributes` keeps every file LF for this reason.
  Development databases migrated from a checkout older than commit `d5dbe2f`
  (2026-09-24, when line endings were normalised) carry a different checksum
  for `0006` and are refused by every later image; no release existed then, so
  only local databases are affected, and they are recreated, not patched.

## Upgrading

The order is the same on Compose and Kubernetes. Each step can be stopped
before the next one without leaving the system half-upgraded.

1. **Read the release.** Its notes (the `CHANGELOG.md` section) name every
   migration, configuration change and whether the migrations are expand-only.
   Check the compatibility matrix above.
2. **Pin and verify the image by digest.** The release notes record the
   digest; verify its signature ([below](#verifying-an-image)) and deploy
   `image@sha256:…`, never a moving tag.
3. **Know your restore point.** Confirm the latest base backup and that WAL
   archiving is current, and write down the time just before step 4
   ([Backup and restore](backup-and-restore.md)).
4. **Migrate only.** Run the new image once, as a login role in
   `gateway_migrator`, with both variables set. `GATEWAY_MIGRATE_ONLY` alone
   does not migrate; it only stops the process after the migration step.

   ```sh
   docker run --rm \
     -e GATEWAY_DATABASE_URL='postgres://<migrator>@<host>/<db>?sslmode=verify-full' \
     -e GATEWAY_RUN_MIGRATIONS=true -e GATEWAY_MIGRATE_ONLY=true \
     ghcr.io/sskutushev/crypto-gateway-project@sha256:<digest>
   ```

   On Kubernetes this is a one-off Job with the same environment and the
   migrator Secret. The process exits 0 after logging
   `migrations applied; GATEWAY_MIGRATE_ONLY is set, exiting`.
5. **Grants.** As the database administrator, apply `db/roles/00_roles.sql`
   if the release changed it, then `db/roles/10_grants.sql`. Re-apply grants
   after every migration: a new table has no grants until then, and the
   processes that need it are refused.
6. **Roll the API.** `/health/ready` must answer `200` through the edge; a
   `503` body names the self-check row that disagrees.
7. **Roll the workers**: expiry, observers, verifier, settlement, outbox,
   reconciler. Each takes its lease only after its own self-check passes.
8. **Watch one reconciliation interval.** `gateway_reconciliation_last_run_status`
   is `0`, `gateway_rail_stops_open` is `0`, and `gateway_outbox_events_pending`
   is not growing.

Between steps 4 and 7 the old processes run against the new schema. That is
safe only because a release's migrations are expand-only (the rule for
authors below); a release whose notes say otherwise stops every process
before step 4 and accepts the downtime.

## Rolling back

There are no down migrations. What can be undone depends on how far the
upgrade got.

| The upgrade stopped | Roll back by |
|---|---|
| before step 4 (no migration ran) | redeploying the previous digest. Nothing else changed. |
| after step 4, the release is expand-only | redeploying the previous digest for the API and the workers, with `GATEWAY_RUN_MIGRATIONS=false`. The new tables and columns stay and are ignored; the next upgrade finds them already applied. |
| after step 4, the release is not expand-only | rolling forward with a fixed release. The previous image cannot serve against the new schema. |
| a migration damaged data | rolling forward with a corrective migration if the damage can be computed from what is left; a point-in-time restore to the time written down in step 3 only as a last resort, because it discards every payment settled and every event written after it. Then follow [what to check after a restore](backup-and-restore.md#after-a-restore). |

Never delete rows from `_sqlx_migrations`, revert a migration by hand, or run
an older image with `GATEWAY_RUN_MIGRATIONS=true` against a newer schema (it
refuses, correctly).

**Rule for migration authors.** A column or table is removed in two releases:
the first stops using it (and makes it nullable if the code wrote it), the
second drops it. A new column is nullable or has a default in the release
that adds it. A release note says "expand-only" or explains why it is not.

## Cutting a release

1. `main` is green in CI.
2. A pull request that:
   - moves the `Unreleased` entries of `CHANGELOG.md` into
     `## [X.Y.Z] - YYYY-MM-DD`, naming the migrations it adds, whether they
     are expand-only, and any configuration change;
   - sets `[workspace.package] version` in `Cargo.toml` to `X.Y.Z` (and
     `info.version` in `docs/openapi.json` if the API changed);
   - adds the release's row to the compatibility matrix above.
3. After it merges, tag the merge commit and push the tag:

   ```sh
   git tag -a vX.Y.Z -m vX.Y.Z <merge commit>
   git push origin vX.Y.Z
   ```

4. `ci.yml` runs on the tag: every check, then the `image` job (build,
   SBOM, scan, and a refusal when `CHANGELOG.md` has no `[X.Y.Z]` section or
   the Cargo version differs), then the `publish` job, which exists only on
   a tag and is the only job holding a registry token and an OIDC identity:
   it builds again from the same commit, pushes to GHCR with provenance,
   takes the SBOM and the scan from the published digest itself, signs the
   digest with keyless cosign and attaches the SBOM to it as an attestation.
5. `release.yml` runs on the same tag: it extracts the `[X.Y.Z]` section of
   `CHANGELOG.md` (and fails when it is missing), waits for the `ci` run of
   the tag to succeed, resolves the image digest, verifies its signature, and
   creates the GitHub Release with the notes, the digest and the SBOM
   (`sbom.spdx.json`) attached.

The release notes can be previewed before tagging:
`python scripts/release-notes.py X.Y.Z`.

If either workflow fails on the tag, fix the cause on `main` and cut the next
PATCH version. A published tag is never moved: someone may already have
pulled its image.

## Verifying an image

Images are signed by the `ci` workflow with [cosign](https://docs.sigstore.dev/)
keyless signing: Sigstore issues the certificate to the GitHub Actions
identity of the workflow and the tag, and the signature is recorded in the
public Rekor log. There is no signing key to leak or rotate.

```sh
cosign verify \
  ghcr.io/sskutushev/crypto-gateway-project@sha256:<digest from the release notes> \
  --certificate-identity 'https://github.com/Sskutushev/crypto-gateway-project/.github/workflows/ci.yml@refs/tags/vX.Y.Z' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

To accept any release tag of this repository rather than one version, use
`--certificate-identity-regexp '^https://github\.com/Sskutushev/crypto-gateway-project/\.github/workflows/ci\.yml@refs/tags/v[0-9]+\.[0-9]+\.[0-9]+$'`.
A signature made by a fork, another workflow or a branch build fails both
forms.

The SBOM attached to the release is SPDX JSON, generated from the published
digest itself; the same document is attached to the image as a cosign
attestation and can be read from the registry without the release:

```sh
cosign verify-attestation --type spdxjson \
  ghcr.io/sskutushev/crypto-gateway-project@sha256:<digest> \
  --certificate-identity 'https://github.com/Sskutushev/crypto-gateway-project/.github/workflows/ci.yml@refs/tags/vX.Y.Z' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

Every GitHub Action the workflows run is pinned to a commit SHA (the version
is the comment beside it), and both Docker base images are pinned by digest;
a moved tag or branch upstream cannot change what a release is built with.
Updates to those pins arrive as their own pull requests.
