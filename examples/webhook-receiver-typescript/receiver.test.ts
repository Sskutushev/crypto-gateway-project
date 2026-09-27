import assert from "node:assert/strict";
import type { AddressInfo } from "node:net";
import { test } from "node:test";

import { createReceiver, InMemoryOrders } from "./server.ts";
import { computeSignature, parseSignatureHeader, verifySignature } from "./verify.ts";

// The vector is derived exactly as crates/gateway-domain/src/webhook.rs does:
//   master key   = 32 bytes of 0x07
//   merchant id  = Uuid::from_u128(1), endpoint id = Uuid::from_u128(2)
//   secret(v)    = hex(HMAC-SHA256(master, "gateway-webhook-secret-v1"
//                    || v as i32 big-endian || merchant bytes || endpoint bytes))
// SECRET_V1 is the value pinned by the_signature_is_stable_across_releases.
// The body is the envelope serde_json emits for a payment_intent.paid event
// (keys sorted, no whitespace); the signature is
//   hex(HMAC-SHA256(hex_decode(secret), "1700000000." || body)).
const SECRET_V1 = "6187fe179459663943dbad68a397beea67e72e6194e31e0a8eb7d4b921267fe2";
const SECRET_V2 = "825c1d2c393df266b4234708f316bff2b2a49f5277516bb6eee4b90111b45182";
const TIMESTAMP = 1_700_000_000;
const BODY = Buffer.from(
  '{"created_at":1700000000,"data":{"attributes":{"attempt_id":"00000000-0000-0000-0000-000000000005",' +
    '"payment_intent_id":"00000000-0000-0000-0000-000000000004","transfer_id":"00000000-0000-0000-0000-000000000006"},' +
    '"id":"00000000-0000-0000-0000-000000000004","object":"payment_intent"},' +
    '"id":"00000000-0000-0000-0000-000000000003","type":"payment_intent.paid"}',
  "utf8",
);
const SIGNATURE_V1 = "8e539fc858dc0c9c69c22a9f8504e69978e68fc3f3ae98164915456bd91b5467";
const SIGNATURE_V2 = "30b6dcd1b271a6d3beced219c3730370d90588c708d60af610611756f3cd26c5";
const HEADER = `t=${TIMESTAMP},v1=${SIGNATURE_V1}`;

test("the signature matches the fixed vector", () => {
  assert.equal(computeSignature(SECRET_V1, TIMESTAMP, BODY).toString("hex"), SIGNATURE_V1);
  assert.equal(computeSignature(SECRET_V2, TIMESTAMP, BODY).toString("hex"), SIGNATURE_V2);
});

test("a valid delivery inside the tolerance is accepted", () => {
  const result = verifySignature(HEADER, BODY, { secrets: [SECRET_V1], nowSeconds: TIMESTAMP + 10 });
  assert.deepEqual(result, { ok: true, timestamp: TIMESTAMP });
});

test("a changed body, a wrong secret or a changed timestamp is refused", () => {
  const tampered = Buffer.from(BODY.toString("utf8").replace("paid", "lost"), "utf8");
  assert.equal(verifySignature(HEADER, tampered, { secrets: [SECRET_V1], nowSeconds: TIMESTAMP }).ok, false);
  assert.equal(verifySignature(HEADER, BODY, { secrets: [SECRET_V2], nowSeconds: TIMESTAMP }).ok, false);
  const shifted = `t=${TIMESTAMP + 1},v1=${SIGNATURE_V1}`;
  assert.equal(verifySignature(shifted, BODY, { secrets: [SECRET_V1], nowSeconds: TIMESTAMP }).ok, false);
});

test("a delivery older or newer than the tolerance is refused", () => {
  for (const now of [TIMESTAMP + 301, TIMESTAMP - 301]) {
    assert.deepEqual(verifySignature(HEADER, BODY, { secrets: [SECRET_V1], nowSeconds: now }), {
      ok: false,
      reason: "timestamp_outside_tolerance",
    });
  }
  const strict = verifySignature(HEADER, BODY, { secrets: [SECRET_V1], nowSeconds: TIMESTAMP + 61, toleranceSeconds: 60 });
  assert.equal(strict.ok, false);
});

test("several v1 values are accepted when any one matches", () => {
  const rotating = `t=${TIMESTAMP},v1=${SIGNATURE_V2},v1=${SIGNATURE_V1}`;
  assert.equal(verifySignature(rotating, BODY, { secrets: [SECRET_V1], nowSeconds: TIMESTAMP }).ok, true);
  assert.equal(verifySignature(rotating, BODY, { secrets: [SECRET_V2], nowSeconds: TIMESTAMP }).ok, true);
  const wrong = `t=${TIMESTAMP},v1=${"0".repeat(64)},v1=${"f".repeat(64)}`;
  assert.equal(verifySignature(wrong, BODY, { secrets: [SECRET_V1], nowSeconds: TIMESTAMP }).ok, false);
});

test("a previous local secret still verifies during a rotation", () => {
  assert.equal(verifySignature(HEADER, BODY, { secrets: [SECRET_V2, SECRET_V1], nowSeconds: TIMESTAMP }).ok, true);
});

test("malformed headers are refused", () => {
  for (const header of [
    "",
    "v1=" + SIGNATURE_V1,
    `t=${TIMESTAMP}`,
    `t=${TIMESTAMP},t=${TIMESTAMP},v1=${SIGNATURE_V1}`,
    `t=abc,v1=${SIGNATURE_V1}`,
    `t=${TIMESTAMP},v1=${SIGNATURE_V1.slice(2)}`,
    `t=${TIMESTAMP},v1=${SIGNATURE_V1.toUpperCase()}`,
    `t=${TIMESTAMP},garbage`,
  ]) {
    assert.equal(verifySignature(header, BODY, { secrets: [SECRET_V1], nowSeconds: TIMESTAMP }).ok, false, header);
  }
  assert.deepEqual(parseSignatureHeader(`t=${TIMESTAMP},v0=x,v1=${SIGNATURE_V1}`), {
    timestamp: TIMESTAMP,
    signatures: [SIGNATURE_V1],
  });
});

test("the receiver fulfils once, acknowledges a replay and refuses a forgery", async () => {
  const orders = new InMemoryOrders();
  const server = createReceiver({
    secrets: [SECRET_V1],
    handler: orders,
    nowSeconds: () => TIMESTAMP,
    log: () => undefined,
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address() as AddressInfo;
  const url = `http://127.0.0.1:${port}/webhooks/gateway`;
  const send = (signature: string) =>
    fetch(url, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "gateway-signature": signature,
        "gateway-event-id": "00000000-0000-0000-0000-000000000003",
      },
      body: BODY,
    });

  try {
    const first = await send(HEADER);
    assert.equal(first.status, 200);
    assert.equal(await first.text(), "ok");
    const replay = await send(HEADER);
    assert.equal(replay.status, 200);
    assert.equal(await replay.text(), "duplicate");
    const forged = await send(`t=${TIMESTAMP},v1=${"0".repeat(64)}`);
    assert.equal(forged.status, 400);
    await forged.text();
    assert.deepEqual([...orders.fulfilledIntents], ["00000000-0000-0000-0000-000000000004"]);
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
});
