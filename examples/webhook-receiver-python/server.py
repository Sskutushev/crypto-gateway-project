"""A minimal gateway webhook receiver. Standard library only; Python 3.10+."""

from __future__ import annotations

import json
import logging
import os
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Callable, Protocol, Sequence

from verify import DEFAULT_TOLERANCE_SECONDS, verify_signature

log = logging.getLogger("webhook")

MAX_BODY_BYTES = 1024 * 1024


class EventHandler(Protocol):
    def handle(self, event: dict[str, Any]) -> None:
        """Business handling. It must be durable before it returns."""


class InMemoryProcessedEvents:
    """Event ids already handled.

    Kept in memory, so it is lost on restart and not shared between
    processes. Production must persist event ids, in the same database
    transaction as the business effect, under a unique constraint.
    """

    def __init__(self) -> None:
        self._states: dict[str, str] = {}
        self._lock = threading.Lock()

    def begin(self, event_id: str) -> str:
        with self._lock:
            current = self._states.get(event_id)
            if current is not None:
                return current
            self._states[event_id] = "processing"
            return "started"

    def finish(self, event_id: str) -> None:
        with self._lock:
            self._states[event_id] = "done"

    def abandon(self, event_id: str) -> None:
        with self._lock:
            self._states.pop(event_id, None)


class InMemoryOrders:
    """Example business logic: only ``payment_intent.paid`` fulfils."""

    def __init__(self) -> None:
        self.fulfilled_intents: set[str] = set()
        self.partial_payments: list[dict[str, Any]] = []
        self.overpayments: list[dict[str, Any]] = []
        self.ignored: list[dict[str, Any]] = []
        self._lock = threading.Lock()

    def handle(self, event: dict[str, Any]) -> None:
        with self._lock:
            kind = event["type"]
            if kind == "payment_intent.paid":
                # Keyed by the payment intent too, so a second paid event for
                # one intent cannot ship twice.
                intent_id = event["data"]["id"]
                if intent_id in self.fulfilled_intents:
                    return
                # Ship the order here, then record it, in one durable transaction.
                self.fulfilled_intents.add(intent_id)
            elif kind == "payment_intent.partially_paid":
                # Money arrived but the obligation is not covered. Do not fulfil.
                self.partial_payments.append(event)
            elif kind == "OVERPAID":
                # Sent with payment_intent.paid when more than the amount
                # arrived; attributes.remainder_raw is for a person to settle
                # outside the gateway. It never fulfils anything by itself.
                self.overpayments.append(event)
            else:
                # Acknowledged so an unknown type is not retried until it is
                # dead-lettered; kept so it can be reviewed.
                self.ignored.append(event)


def _is_event(value: Any) -> bool:
    if not isinstance(value, dict):
        return False
    data = value.get("data")
    return (
        isinstance(value.get("id"), str)
        and isinstance(value.get("type"), str)
        and isinstance(value.get("created_at"), int)
        and isinstance(data, dict)
        and isinstance(data.get("object"), str)
        and isinstance(data.get("id"), str)
        and isinstance(data.get("attributes"), dict)
    )


def make_handler(
    secrets: Sequence[str],
    handler: EventHandler,
    processed: InMemoryProcessedEvents | None = None,
    tolerance_seconds: int = DEFAULT_TOLERANCE_SECONDS,
    path: str = "/webhooks/gateway",
    now_seconds: Callable[[], int] | None = None,
) -> type[BaseHTTPRequestHandler]:
    processed_events = processed if processed is not None else InMemoryProcessedEvents()

    class WebhookRequestHandler(BaseHTTPRequestHandler):
        def log_message(self, format: str, *args: Any) -> None:  # noqa: A002
            log.debug(format, *args)

        def _reply(self, status: int, body: str) -> None:
            encoded = body.encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

        def do_GET(self) -> None:  # noqa: N802
            self._reply(405 if self.path == path else 404, "method not allowed")

        def do_POST(self) -> None:  # noqa: N802
            if self.path != path:
                self._reply(404, "not found")
                return
            try:
                length = int(self.headers.get("Content-Length", ""))
            except ValueError:
                self._reply(411, "length required")
                return
            if length < 0 or length > MAX_BODY_BYTES:
                self._reply(413, "body too large")
                return
            raw = self.rfile.read(length)

            # Verification runs on the raw bytes, before any parsing.
            result = verify_signature(
                self.headers.get("Gateway-Signature"),
                raw,
                secrets,
                tolerance_seconds=tolerance_seconds,
                now_seconds=now_seconds() if now_seconds else None,
            )
            if not result.ok:
                log.warning("webhook_rejected reason=%s", result.reason)
                self._reply(400, "invalid signature")
                return

            try:
                event = json.loads(raw.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError):
                event = None
            if not _is_event(event):
                self._reply(400, "malformed event")
                return
            header_id = self.headers.get("Gateway-Event-Id")
            if header_id is not None and header_id != event["id"]:
                self._reply(400, "event id mismatch")
                return

            state = processed_events.begin(event["id"])
            if state == "done":
                # Delivery is at least once; a repeat is answered 2xx so it stops.
                self._reply(200, "duplicate")
                return
            if state == "processing":
                # A concurrent copy; a non-2xx makes the gateway retry later.
                self._reply(409, "in progress")
                return

            try:
                handler.handle(event)
            except Exception:
                processed_events.abandon(event["id"])
                log.exception("webhook_handler_failed event=%s", event["id"])
                self._reply(500, "handler failed")
                return
            processed_events.finish(event["id"])
            log.info("webhook_handled event=%s type=%s", event["id"], event["type"])
            self._reply(200, "ok")

    return WebhookRequestHandler


def main() -> None:
    logging.basicConfig(level=logging.INFO)
    secrets = [s.strip() for s in os.environ.get("WEBHOOK_SECRETS", "").split(",") if s.strip()]
    if not secrets:
        raise SystemExit("WEBHOOK_SECRETS must list at least one hex secret, newest first")
    port = int(os.environ.get("PORT", "8080"))
    tolerance = int(os.environ.get("WEBHOOK_TOLERANCE_SECONDS", str(DEFAULT_TOLERANCE_SECONDS)))
    request_handler = make_handler(
        secrets,
        InMemoryOrders(),
        tolerance_seconds=tolerance,
        path=os.environ.get("WEBHOOK_PATH", "/webhooks/gateway"),
    )
    server = ThreadingHTTPServer(("0.0.0.0", port), request_handler)
    log.info("listening on :%d", port)
    server.serve_forever()


if __name__ == "__main__":
    main()
