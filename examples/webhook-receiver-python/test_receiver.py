"""Run with: python -m unittest -v (from this directory)."""

from __future__ import annotations

import http.client
import logging
import threading
import unittest
from http.server import ThreadingHTTPServer

from server import InMemoryOrders, make_handler
from verify import compute_signature, parse_signature_header, verify_signature

# The vector is derived exactly as crates/gateway-domain/src/webhook.rs does:
#   master key   = 32 bytes of 0x07
#   merchant id  = Uuid::from_u128(1), endpoint id = Uuid::from_u128(2)
#   secret(v)    = hex(HMAC-SHA256(master, b"gateway-webhook-secret-v1"
#                    + v as i32 big-endian + merchant bytes + endpoint bytes))
# SECRET_V1 is the value pinned by the_signature_is_stable_across_releases.
# The body is the envelope serde_json emits for a payment_intent.paid event
# (keys sorted, no whitespace); the signature is
#   hex(HMAC-SHA256(bytes.fromhex(secret), b"1700000000." + body)).
SECRET_V1 = "6187fe179459663943dbad68a397beea67e72e6194e31e0a8eb7d4b921267fe2"
SECRET_V2 = "825c1d2c393df266b4234708f316bff2b2a49f5277516bb6eee4b90111b45182"
TIMESTAMP = 1_700_000_000
BODY = (
    b'{"created_at":1700000000,"data":{"attributes":{"attempt_id":"00000000-0000-0000-0000-000000000005",'
    b'"payment_intent_id":"00000000-0000-0000-0000-000000000004","transfer_id":"00000000-0000-0000-0000-000000000006"},'
    b'"id":"00000000-0000-0000-0000-000000000004","object":"payment_intent"},'
    b'"id":"00000000-0000-0000-0000-000000000003","type":"payment_intent.paid"}'
)
SIGNATURE_V1 = "8e539fc858dc0c9c69c22a9f8504e69978e68fc3f3ae98164915456bd91b5467"
SIGNATURE_V2 = "30b6dcd1b271a6d3beced219c3730370d90588c708d60af610611756f3cd26c5"
HEADER = f"t={TIMESTAMP},v1={SIGNATURE_V1}"


class SignatureTests(unittest.TestCase):
    def test_the_signature_matches_the_fixed_vector(self) -> None:
        self.assertEqual(compute_signature(SECRET_V1, TIMESTAMP, BODY).hex(), SIGNATURE_V1)
        self.assertEqual(compute_signature(SECRET_V2, TIMESTAMP, BODY).hex(), SIGNATURE_V2)

    def test_a_valid_delivery_inside_the_tolerance_is_accepted(self) -> None:
        result = verify_signature(HEADER, BODY, [SECRET_V1], now_seconds=TIMESTAMP + 10)
        self.assertTrue(result.ok)
        self.assertEqual(result.timestamp, TIMESTAMP)

    def test_a_changed_body_secret_or_timestamp_is_refused(self) -> None:
        tampered = BODY.replace(b"paid", b"lost")
        self.assertFalse(verify_signature(HEADER, tampered, [SECRET_V1], now_seconds=TIMESTAMP).ok)
        self.assertFalse(verify_signature(HEADER, BODY, [SECRET_V2], now_seconds=TIMESTAMP).ok)
        shifted = f"t={TIMESTAMP + 1},v1={SIGNATURE_V1}"
        self.assertFalse(verify_signature(shifted, BODY, [SECRET_V1], now_seconds=TIMESTAMP).ok)

    def test_a_delivery_outside_the_tolerance_is_refused(self) -> None:
        for now in (TIMESTAMP + 301, TIMESTAMP - 301):
            result = verify_signature(HEADER, BODY, [SECRET_V1], now_seconds=now)
            self.assertEqual(result.reason, "timestamp_outside_tolerance")
        strict = verify_signature(HEADER, BODY, [SECRET_V1], tolerance_seconds=60, now_seconds=TIMESTAMP + 61)
        self.assertFalse(strict.ok)

    def test_several_v1_values_are_accepted_when_any_one_matches(self) -> None:
        rotating = f"t={TIMESTAMP},v1={SIGNATURE_V2},v1={SIGNATURE_V1}"
        self.assertTrue(verify_signature(rotating, BODY, [SECRET_V1], now_seconds=TIMESTAMP).ok)
        self.assertTrue(verify_signature(rotating, BODY, [SECRET_V2], now_seconds=TIMESTAMP).ok)
        wrong = f"t={TIMESTAMP},v1={'0' * 64},v1={'f' * 64}"
        self.assertFalse(verify_signature(wrong, BODY, [SECRET_V1], now_seconds=TIMESTAMP).ok)

    def test_a_previous_local_secret_still_verifies_during_a_rotation(self) -> None:
        self.assertTrue(verify_signature(HEADER, BODY, [SECRET_V2, SECRET_V1], now_seconds=TIMESTAMP).ok)

    def test_malformed_headers_are_refused(self) -> None:
        for header in (
            "",
            f"v1={SIGNATURE_V1}",
            f"t={TIMESTAMP}",
            f"t={TIMESTAMP},t={TIMESTAMP},v1={SIGNATURE_V1}",
            f"t=abc,v1={SIGNATURE_V1}",
            f"t={TIMESTAMP},v1={SIGNATURE_V1[2:]}",
            f"t={TIMESTAMP},v1={SIGNATURE_V1.upper()}",
            f"t={TIMESTAMP},garbage",
        ):
            with self.subTest(header=header):
                self.assertFalse(verify_signature(header, BODY, [SECRET_V1], now_seconds=TIMESTAMP).ok)
        parsed = parse_signature_header(f"t={TIMESTAMP},v0=x,v1={SIGNATURE_V1}")
        self.assertIsNotNone(parsed)
        assert parsed is not None
        self.assertEqual(parsed.signatures, (SIGNATURE_V1,))


class ReceiverTests(unittest.TestCase):
    def test_the_receiver_fulfils_once_acknowledges_a_replay_and_refuses_a_forgery(self) -> None:
        logging.getLogger("webhook").setLevel(logging.CRITICAL)
        orders = InMemoryOrders()
        handler = make_handler([SECRET_V1], orders, now_seconds=lambda: TIMESTAMP)
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        port = server.server_address[1]

        def send(signature: str) -> tuple[int, str]:
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
            try:
                connection.request(
                    "POST",
                    "/webhooks/gateway",
                    body=BODY,
                    headers={
                        "Content-Type": "application/json",
                        "Gateway-Signature": signature,
                        "Gateway-Event-Id": "00000000-0000-0000-0000-000000000003",
                    },
                )
                response = connection.getresponse()
                return response.status, response.read().decode("utf-8")
            finally:
                connection.close()

        try:
            self.assertEqual(send(HEADER), (200, "ok"))
            self.assertEqual(send(HEADER), (200, "duplicate"))
            self.assertEqual(send(f"t={TIMESTAMP},v1={'0' * 64}")[0], 400)
            self.assertEqual(orders.fulfilled_intents, {"00000000-0000-0000-0000-000000000004"})
        finally:
            server.shutdown()
            server.server_close()


if __name__ == "__main__":
    unittest.main()
