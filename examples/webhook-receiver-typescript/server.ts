import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { pathToFileURL } from "node:url";

import { DEFAULT_TOLERANCE_SECONDS, verifySignature } from "./verify.ts";

/** The envelope the gateway posts. Field order in the JSON is not significant. */
export interface GatewayEvent {
  id: string;
  type: string;
  created_at: number;
  data: {
    object: string;
    id: string;
    attributes: Record<string, unknown>;
  };
}

/** Business handling. It must be durable before it resolves. */
export interface EventHandler {
  handle(event: GatewayEvent): Promise<void>;
}

/**
 * Event ids already handled. This example keeps them in memory, which is lost
 * on restart and not shared between instances. Production must persist them,
 * in the same database transaction as the business effect, under a unique
 * constraint on the event id.
 */
export class InMemoryProcessedEvents {
  private readonly states = new Map<string, "processing" | "done">();

  /** Marks the event as in progress unless it is already known. */
  begin(eventId: string): "started" | "processing" | "done" {
    const current = this.states.get(eventId);
    if (current !== undefined) {
      return current;
    }
    this.states.set(eventId, "processing");
    return "started";
  }

  finish(eventId: string): void {
    this.states.set(eventId, "done");
  }

  abandon(eventId: string): void {
    this.states.delete(eventId);
  }
}

/**
 * Example business logic. `payment_intent.paid` is the only event that
 * fulfils an order; the other events are recorded for people to look at.
 */
export class InMemoryOrders implements EventHandler {
  readonly fulfilledIntents = new Set<string>();
  readonly partialPayments: GatewayEvent[] = [];
  readonly overpayments: GatewayEvent[] = [];
  readonly ignored: GatewayEvent[] = [];

  async handle(event: GatewayEvent): Promise<void> {
    switch (event.type) {
      case "payment_intent.paid": {
        // Fulfilment is keyed by the payment intent too, not only by the
        // event id, so a second paid event for one intent cannot ship twice.
        if (this.fulfilledIntents.has(event.data.id)) {
          return;
        }
        // Ship the order here, then record it, inside one durable transaction.
        this.fulfilledIntents.add(event.data.id);
        return;
      }
      case "payment_intent.partially_paid":
        // Money arrived but the obligation is not covered. Do not fulfil.
        this.partialPayments.push(event);
        return;
      case "OVERPAID":
        // Sent together with payment_intent.paid when more than the amount
        // arrived. attributes.remainder_raw is the excess, for a person to
        // settle outside the gateway. It never fulfils anything by itself.
        this.overpayments.push(event);
        return;
      default:
        // Acknowledged so an event type this receiver does not know yet is
        // not retried until dead-lettered; kept so it can be reviewed.
        this.ignored.push(event);
    }
  }
}

export interface ReceiverOptions {
  secrets: readonly string[];
  handler: EventHandler;
  processed?: InMemoryProcessedEvents;
  toleranceSeconds?: number;
  path?: string;
  maxBodyBytes?: number;
  nowSeconds?: () => number;
  log?: (message: string) => void;
}

class BodyTooLarge extends Error {}

async function readRawBody(request: IncomingMessage, limit: number): Promise<Buffer> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of request) {
    const buffer = chunk as Buffer;
    size += buffer.length;
    if (size > limit) {
      throw new BodyTooLarge();
    }
    chunks.push(buffer);
  }
  return Buffer.concat(chunks);
}

function asEvent(value: unknown): GatewayEvent | null {
  if (typeof value !== "object" || value === null) {
    return null;
  }
  const event = value as Record<string, unknown>;
  const data = event.data as Record<string, unknown> | undefined;
  if (
    typeof event.id !== "string" ||
    typeof event.type !== "string" ||
    typeof event.created_at !== "number" ||
    typeof data !== "object" ||
    data === null ||
    typeof data.object !== "string" ||
    typeof data.id !== "string" ||
    typeof data.attributes !== "object" ||
    data.attributes === null
  ) {
    return null;
  }
  return event as unknown as GatewayEvent;
}

function reply(response: ServerResponse, status: number, body: string): void {
  response.writeHead(status, { "content-type": "text/plain; charset=utf-8" });
  response.end(body);
}

export function createReceiver(options: ReceiverOptions): Server {
  const path = options.path ?? "/webhooks/gateway";
  const limit = options.maxBodyBytes ?? 1024 * 1024;
  const processed = options.processed ?? new InMemoryProcessedEvents();
  const log = options.log ?? ((message: string) => console.log(message));

  return createServer(async (request, response) => {
    if (request.url !== path) {
      reply(response, 404, "not found");
      return;
    }
    if (request.method !== "POST") {
      reply(response, 405, "method not allowed");
      return;
    }

    let raw: Buffer;
    try {
      raw = await readRawBody(request, limit);
    } catch (error) {
      if (error instanceof BodyTooLarge) {
        reply(response, 413, "body too large");
        return;
      }
      log(`webhook_body_read_failed error=${String(error)}`);
      reply(response, 400, "body could not be read");
      return;
    }

    // Verification runs on the raw bytes. Parsing first and re-serialising
    // would change the bytes and break the signature.
    const verified = verifySignature(request.headers["gateway-signature"] as string | undefined, raw, {
      secrets: options.secrets,
      toleranceSeconds: options.toleranceSeconds ?? DEFAULT_TOLERANCE_SECONDS,
      nowSeconds: options.nowSeconds?.(),
    });
    if (!verified.ok) {
      log(`webhook_rejected reason=${verified.reason}`);
      reply(response, 400, "invalid signature");
      return;
    }

    let event: GatewayEvent | null;
    try {
      event = asEvent(JSON.parse(raw.toString("utf8")));
    } catch {
      event = null;
    }
    if (event === null) {
      reply(response, 400, "malformed event");
      return;
    }
    const headerId = request.headers["gateway-event-id"];
    if (headerId !== undefined && headerId !== event.id) {
      reply(response, 400, "event id mismatch");
      return;
    }

    const state = processed.begin(event.id);
    if (state === "done") {
      // Delivery is at least once. A repeat is answered 2xx so it stops.
      reply(response, 200, "duplicate");
      return;
    }
    if (state === "processing") {
      // A concurrent delivery of the same event. A non-2xx makes the gateway
      // try again later instead of treating this copy as handled.
      reply(response, 409, "in progress");
      return;
    }

    try {
      await options.handler.handle(event);
    } catch (error) {
      processed.abandon(event.id);
      log(`webhook_handler_failed event=${event.id} error=${String(error)}`);
      reply(response, 500, "handler failed");
      return;
    }
    processed.finish(event.id);
    log(`webhook_handled event=${event.id} type=${event.type}`);
    reply(response, 200, "ok");
  });
}

function secretsFromEnvironment(): string[] {
  const raw = process.env.WEBHOOK_SECRETS ?? "";
  const secrets = raw
    .split(",")
    .map((value) => value.trim())
    .filter((value) => value.length > 0);
  if (secrets.length === 0) {
    throw new Error("WEBHOOK_SECRETS must list at least one hex secret, newest first");
  }
  return secrets;
}

const invokedDirectly =
  process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href;

if (invokedDirectly) {
  const port = Number(process.env.PORT ?? "8080");
  const tolerance = Number(process.env.WEBHOOK_TOLERANCE_SECONDS ?? String(DEFAULT_TOLERANCE_SECONDS));
  const server = createReceiver({
    secrets: secretsFromEnvironment(),
    handler: new InMemoryOrders(),
    toleranceSeconds: tolerance,
    path: process.env.WEBHOOK_PATH ?? "/webhooks/gateway",
  });
  server.listen(port, () => {
    console.log(`listening on :${port}`);
  });
}
