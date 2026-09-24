# Security policy

## Reporting a vulnerability

Report privately through GitHub's private vulnerability reporting on this
repository (Security → Report a vulnerability). Do not open a public issue.
You will get an acknowledgement within three working days and a decision on
severity and a fix window within ten.

If private reporting is unavailable to you, open an issue that says only
"security: please contact me" and a maintainer will reach out.

## Supported versions

The default branch and the latest tagged release. Older tags are not patched.

## What counts as a vulnerability here

The gateway holds no keys, so the interesting failures are about being lied to
or about money being counted wrong:

- a way to make a canonical transfer from fewer independent sources than the
  finality policy requires, or from a single observer;
- a way to settle an obligation twice, allocate more than a transfer carried,
  or allocate one transfer to two obligations;
- an amount that passes through a float, wraps, or is parsed with a sign,
  decimals or whitespace;
- a webhook signature that can be forged, replayed outside its timestamp
  window, or delivered to an address the merchant did not register;
- a database role that can write outside its grants, or an observer that can
  speak for another source or move its cursor;
- a way to make a process serve with a database that disagrees with its
  configured collectors, assets or chain environment;
- an idempotency replay that returns another merchant's result;
- a quote issued from missing, stale or unhealthy evidence.

Operational hardening suggestions, dependency advisories with no reachable
path, and findings against the development Compose stack are welcome as
ordinary issues.

## Disclosure

Fixes ship as a tagged release with a changelog entry that credits the
reporter, unless they prefer otherwise.
