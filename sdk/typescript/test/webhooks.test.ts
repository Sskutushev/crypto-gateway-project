import assert from "node:assert/strict";
import { describe, test } from "node:test";

import {
  UnknownWebhookEventError,
  WebhookVerificationError,
  computeWebhookSignature,
  parseSignatureHeader,
  signWebhookPayload,
  verifyWebhook,
  type WebhookEvent,
} from "../src/index.js";

// Pinned vectors shared with examples/webhook-receiver-typescript: SECRET_V1 is
// the value crates/gateway-domain/src/webhook.rs pins in
// the_signature_is_stable_across_releases, and BODY is the envelope serde_json
// emits for a payment_intent.paid event. A change to the scheme on either side
// breaks this test.
const SECRET_V1 = "6187fe179459663943dbad68a397beea67e72e6194e31e0a8eb7d4b921267fe2";
const SECRET_V2 = "825c1d2c393df266b4234708f316bff2b2a49f5277516bb6eee4b90111b45182";
const TIMESTAMP = 1_700_000_000;
const BODY =
  '{"created_at":1700000000,"data":{"attributes":{"attempt_id":"00000000-0000-0000-0000-000000000005",' +
  '"payment_intent_id":"00000000-0000-0000-0000-000000000004","transfer_id":"00000000-0000-0000-0000-000000000006"},' +
  '"id":"00000000-0000-0000-0000-000000000004","object":"payment_intent"},' +
  '"id":"00000000-0000-0000-0000-000000000003","type":"payment_intent.paid"}';
const SIGNATURE_V1 = "8e539fc858dc0c9c69c22a9f8504e69978e68fc3f3ae98164915456bd91b5467";
const SIGNATURE_V2 = "30b6dcd1b271a6d3beced219c3730370d90588c708d60af610611756f3cd26c5";
const NOW = new Date(TIMESTAMP * 1000);

function rejects(run: () => unknown, reason: string): void {
  assert.throws(run, (error: unknown) => {
    assert.ok(error instanceof WebhookVerificationError, `expected WebhookVerificationError, got ${String(error)}`);
    assert.equal(error.reason, reason);
    return true;
  });
}

describe("signature scheme", () => {
  test("computes the gateway's pinned signatures", () => {
    assert.equal(computeWebhookSignature(SECRET_V1, TIMESTAMP, BODY), SIGNATURE_V1);
    assert.equal(computeWebhookSignature(SECRET_V2, TIMESTAMP, Buffer.from(BODY)), SIGNATURE_V2);
  });

  test("signs with one v1 per secret, newest first", () => {
    assert.equal(
      signWebhookPayload({ payload: BODY, secrets: [SECRET_V2, SECRET_V1], timestamp: TIMESTAMP }),
      `t=${TIMESTAMP},v1=${SIGNATURE_V2},v1=${SIGNATURE_V1}`,
    );
  });

  test("parses several v1 values and ignores unknown schemes", () => {
    assert.deepEqual(parseSignatureHeader(`t=${TIMESTAMP}, v1=${SIGNATURE_V1} ,v0=zz,v1=${SIGNATURE_V2}`), {
      timestamp: TIMESTAMP,
      signatures: [SIGNATURE_V1, SIGNATURE_V2],
    });
  });

  test("refuses headers without exactly one timestamp and a valid v1", () => {
    for (const header of [
      `v1=${SIGNATURE_V1}`,
      `t=${TIMESTAMP}`,
      `t=${TIMESTAMP},t=${TIMESTAMP},v1=${SIGNATURE_V1}`,
      `t=-1,v1=${SIGNATURE_V1}`,
      `t=${TIMESTAMP},v1=abc`,
      `t=${TIMESTAMP},garbage`,
    ]) {
      assert.equal(parseSignatureHeader(header), null, header);
    }
  });
});

describe("verifyWebhook", () => {
  const header = `t=${TIMESTAMP},v1=${SIGNATURE_V1}`;

  test("accepts a good signature and returns the typed event", () => {
    const event = verifyWebhook({ payload: BODY, header, secrets: [SECRET_V1], now: NOW });
    assert.equal(event.type, "payment_intent.paid");
    assert.equal(event.id, "00000000-0000-0000-0000-000000000003");
    assert.ok(event.type === "payment_intent.paid");
    assert.equal(event.data.attributes.attempt_id, "00000000-0000-0000-0000-000000000005");
  });

  test("accepts the payload as bytes, including a view into a larger buffer", () => {
    const backing = Buffer.concat([Buffer.from("xxxx"), Buffer.from(BODY), Buffer.from("yyyy")]);
    const view = new Uint8Array(backing.buffer, backing.byteOffset + 4, Buffer.byteLength(BODY));
    assert.equal(verifyWebhook({ payload: view, header, secrets: [SECRET_V1], now: NOW }).type, "payment_intent.paid");
  });

  test("refuses a tampered body", () => {
    const payload = BODY.replace("000000000006", "000000000007");
    rejects(() => verifyWebhook({ payload, header, secrets: [SECRET_V1], now: NOW }), "signature_mismatch");
  });

  test("refuses a signature under another secret", () => {
    rejects(() => verifyWebhook({ payload: BODY, header, secrets: [SECRET_V2], now: NOW }), "signature_mismatch");
  });

  test("refuses a changed timestamp, because the timestamp is signed", () => {
    const shifted = `t=${TIMESTAMP + 1},v1=${SIGNATURE_V1}`;
    rejects(() => verifyWebhook({ payload: BODY, header: shifted, secrets: [SECRET_V1], now: NOW }), "signature_mismatch");
  });

  test("refuses an expired delivery and one from the future, and accepts the edge", () => {
    const at = (seconds: number) => new Date(seconds * 1000);
    rejects(
      () => verifyWebhook({ payload: BODY, header, secrets: [SECRET_V1], now: at(TIMESTAMP + 301) }),
      "timestamp_outside_tolerance",
    );
    rejects(
      () => verifyWebhook({ payload: BODY, header, secrets: [SECRET_V1], now: at(TIMESTAMP - 301) }),
      "timestamp_outside_tolerance",
    );
    assert.doesNotThrow(() => verifyWebhook({ payload: BODY, header, secrets: [SECRET_V1], now: at(TIMESTAMP + 300) }));
    rejects(
      () => verifyWebhook({ payload: BODY, header, secrets: [SECRET_V1], toleranceSeconds: 10, now: at(TIMESTAMP + 11) }),
      "timestamp_outside_tolerance",
    );
  });

  test("defaults to the system clock", () => {
    rejects(() => verifyWebhook({ payload: BODY, header, secrets: [SECRET_V1] }), "timestamp_outside_tolerance");
  });

  test("rotation: two v1 values, verified by either the new or the old secret", () => {
    const rotating = `t=${TIMESTAMP},v1=${SIGNATURE_V2},v1=${SIGNATURE_V1}`;
    for (const secrets of [[SECRET_V1], [SECRET_V2], [SECRET_V2, SECRET_V1], [SECRET_V1, SECRET_V2]]) {
      assert.equal(verifyWebhook({ payload: BODY, header: rotating, secrets, now: NOW }).type, "payment_intent.paid");
    }
  });

  test("rotation: the receiver holds both secrets while the gateway still signs with one", () => {
    assert.doesNotThrow(() => verifyWebhook({ payload: BODY, header, secrets: [SECRET_V2, SECRET_V1], now: NOW }));
  });

  test("missing and malformed headers", () => {
    rejects(() => verifyWebhook({ payload: BODY, header: undefined, secrets: [SECRET_V1], now: NOW }), "missing_header");
    rejects(() => verifyWebhook({ payload: BODY, header: "", secrets: [SECRET_V1], now: NOW }), "missing_header");
    rejects(() => verifyWebhook({ payload: BODY, header: null, secrets: [SECRET_V1], now: NOW }), "missing_header");
    rejects(() => verifyWebhook({ payload: BODY, header: "v1=nope", secrets: [SECRET_V1], now: NOW }), "malformed_header");
    rejects(
      () => verifyWebhook({ payload: BODY, header: [header, header], secrets: [SECRET_V1], now: NOW }),
      "malformed_header",
    );
    assert.equal(verifyWebhook({ payload: BODY, header: [header], secrets: [SECRET_V1], now: NOW }).type, "payment_intent.paid");
  });

  test("a misconfigured receiver is a TypeError, not a verdict", () => {
    assert.throws(() => verifyWebhook({ payload: BODY, header, secrets: [], now: NOW }), TypeError);
    assert.throws(() => verifyWebhook({ payload: BODY, header, secrets: ["not-hex"], now: NOW }), TypeError);
    assert.throws(() => verifyWebhook({ payload: BODY, header, secrets: [SECRET_V1.slice(2)], now: NOW }), TypeError);
  });

  test("a validly signed body that is not an event is refused", () => {
    const emptyAttributes =
      '{"id":"x","type":"payment_intent.paid","created_at":1,"data":{"object":"payment_intent","id":"x","attributes":{}}}';
    for (const payload of ["not json", "[]", '{"id":"x"}', emptyAttributes]) {
      const signed = signWebhookPayload({ payload, secrets: [SECRET_V1], timestamp: TIMESTAMP });
      rejects(() => verifyWebhook({ payload, header: signed, secrets: [SECRET_V1], now: NOW }), "invalid_payload");
    }
    const notUtf8 = Buffer.from([0x7b, 0xff, 0x7d]);
    const signed = signWebhookPayload({ payload: notUtf8, secrets: [SECRET_V1], timestamp: TIMESTAMP });
    rejects(() => verifyWebhook({ payload: notUtf8, header: signed, secrets: [SECRET_V1], now: NOW }), "invalid_payload");
  });

  test("the signature is checked before the body is parsed", () => {
    rejects(() => verifyWebhook({ payload: "not json", header, secrets: [SECRET_V1], now: NOW }), "signature_mismatch");
  });

  test("an unknown but genuine event type is its own error, carrying the envelope", () => {
    const payload = JSON.stringify({
      id: "e1",
      type: "payment_intent.refunded",
      created_at: TIMESTAMP,
      data: { object: "payment_intent", id: "i1", attributes: {} },
    });
    const signed = signWebhookPayload({ payload, secrets: [SECRET_V1], timestamp: TIMESTAMP });
    assert.throws(
      () => verifyWebhook({ payload, header: signed, secrets: [SECRET_V1], now: NOW }),
      (error: unknown) => error instanceof UnknownWebhookEventError && error.envelope.type === "payment_intent.refunded",
    );
  });
});

describe("every event the gateway emits", () => {
  // Shapes from the Rust writers: settlement.rs (paid, partially_paid,
  // OVERPAID), postgres.rs (cancelled) and provisioning.rs (webhook.test).
  const intent = "00000000-0000-0000-0000-00000000000a";
  const transfer = "00000000-0000-0000-0000-00000000000b";
  const pi = (attributes: Record<string, unknown>) => ({ object: "payment_intent", id: intent, attributes });
  const events = [
    { id: "e1", type: "payment_intent.paid", created_at: TIMESTAMP, data: pi({ payment_intent_id: intent, transfer_id: transfer, attempt_id: "a" }) },
    { id: "e2", type: "payment_intent.partially_paid", created_at: TIMESTAMP, data: pi({ payment_intent_id: intent, transfer_id: transfer }) },
    { id: "e3", type: "payment_intent.cancelled", created_at: TIMESTAMP, data: pi({ payment_intent_id: intent, reason: null }) },
    { id: "e4", type: "payment_intent.cancelled", created_at: TIMESTAMP, data: pi({ payment_intent_id: intent, reason: "customer asked" }) },
    { id: "e5", type: "OVERPAID", created_at: TIMESTAMP, data: pi({ payment_intent_id: intent, transfer_id: transfer, remainder_raw: "90071992547409930000" }) },
    { id: "e6", type: "webhook.test", created_at: TIMESTAMP, data: { object: "webhook_endpoint", id: "ep", attributes: { endpoint_id: "ep", note: "A test event." } } },
  ] as unknown as WebhookEvent[];

  const verify = (event: unknown): WebhookEvent => {
    const payload = JSON.stringify(event);
    const signed = signWebhookPayload({ payload, secrets: [SECRET_V1], timestamp: TIMESTAMP });
    return verifyWebhook({ payload, header: signed, secrets: [SECRET_V1], now: NOW });
  };

  for (const event of events) {
    test(`${event.type} (${event.id}) verifies and round-trips`, () => {
      assert.deepEqual(verify(event), event);
    });
  }

  test("OVERPAID keeps remainder_raw a string beyond 2^53", () => {
    const event = verify(events[4]);
    assert.ok(event.type === "OVERPAID");
    assert.equal(event.data.attributes.remainder_raw, "90071992547409930000");
  });

  test("an event with the wrong data object is refused", () => {
    const test5 = events[5]!;
    rejects(() => verify({ ...test5, data: { ...test5.data, object: "payment_intent" } }), "invalid_payload");
  });
});
