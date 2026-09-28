"""Signature verification for gateway webhooks. Standard library only."""

from __future__ import annotations

import hashlib
import hmac
import re
import time
from dataclasses import dataclass
from typing import Sequence

DEFAULT_TOLERANCE_SECONDS = 300

_SECRET = re.compile(r"^[0-9a-fA-F]{64}$")
_SIGNATURE = re.compile(r"^[0-9a-f]{64}$")
_TIMESTAMP = re.compile(r"^[0-9]{1,15}$")


@dataclass(frozen=True)
class ParsedSignature:
    timestamp: int
    signatures: tuple[str, ...]


@dataclass(frozen=True)
class VerifyResult:
    ok: bool
    reason: str | None = None
    timestamp: int | None = None


def parse_signature_header(header: str) -> ParsedSignature | None:
    """Parses ``t=<unix>,v1=<hex>[,v1=<hex>...]``.

    Unknown keys are ignored so a future scheme can be added beside ``v1``.
    Exactly one ``t`` and at least one well-formed ``v1`` are required.
    """
    timestamp: int | None = None
    signatures: list[str] = []
    for part in header.split(","):
        key, separator, value = part.partition("=")
        key, value = key.strip(), value.strip()
        if not separator or not key:
            return None
        if key == "t":
            if timestamp is not None or not _TIMESTAMP.match(value):
                return None
            timestamp = int(value)
        elif key == "v1":
            if not _SIGNATURE.match(value):
                return None
            signatures.append(value)
    if timestamp is None or not signatures:
        return None
    return ParsedSignature(timestamp, tuple(signatures))


def compute_signature(secret_hex: str, timestamp: int, raw_body: bytes) -> bytes:
    """HMAC-SHA256 over ``<t>.<raw body>``, keyed by the hex-decoded secret.

    The key is the 32 decoded bytes, not the 64-character hex text.
    """
    if not _SECRET.match(secret_hex):
        raise ValueError("a webhook secret must be 64 hex characters")
    message = str(timestamp).encode("ascii") + b"." + raw_body
    return hmac.new(bytes.fromhex(secret_hex), message, hashlib.sha256).digest()


def verify_signature(
    header: str | None,
    raw_body: bytes,
    secrets: Sequence[str],
    tolerance_seconds: int = DEFAULT_TOLERANCE_SECONDS,
    now_seconds: int | None = None,
) -> VerifyResult:
    """Verifies the header against the exact bytes received.

    ``secrets`` lists every secret this receiver trusts, newest first; keep
    the previous one during a rotation. Valid when any secret matches any
    ``v1`` value.
    """
    if not header:
        return VerifyResult(False, "missing_header")
    if not secrets:
        return VerifyResult(False, "no_secret_configured")
    parsed = parse_signature_header(header)
    if parsed is None:
        return VerifyResult(False, "malformed_header")
    now = int(time.time()) if now_seconds is None else now_seconds
    if abs(now - parsed.timestamp) > tolerance_seconds:
        return VerifyResult(False, "timestamp_outside_tolerance")

    matched = False
    for secret in secrets:
        expected = compute_signature(secret, parsed.timestamp, raw_body)
        for candidate in parsed.signatures:
            if hmac.compare_digest(expected, bytes.fromhex(candidate)):
                matched = True
    if matched:
        return VerifyResult(True, timestamp=parsed.timestamp)
    return VerifyResult(False, "signature_mismatch")
