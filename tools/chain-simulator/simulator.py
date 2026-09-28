#!/usr/bin/env python3
"""A deterministic, in-memory TRON node for integration testing.

It answers the handful of TRON HTTP calls the gateway's TRON source makes, so
the unmodified gateway pipeline can run end to end with no chain at all. It is
a test double: nothing it answers is a fact about any real chain, and a
deployment that points at it is testing its own wiring, nothing more.

Standard library only; Python 3.10+.

Chain model
-----------
Block ``n`` exists once ``genesis_ms + n * interval_ms`` has passed, so the
head advances by itself every interval and every block's number, id, parent
and timestamp are a pure function of the seed and the configuration. The
solidified head trails the head by ``solid_lag`` blocks, as on TRON. A
transfer submitted through the control API is mined into the block after the
current head and becomes visible when that block is produced; the contents of
a block are frozen before anyone can read it.

Control API (same port, under ``/simulator/``): only a request from a loopback
address, or one carrying the configured ``X-Simulator-Token``, is served.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import ipaddress
import json
import logging
import os
import sys
import threading
import time
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Callable
from urllib.parse import parse_qs, urlsplit

log = logging.getLogger("chain-simulator")

# --------------------------------------------------------------------------
# Base58check, as TRON uses it: 0x41 followed by 20 address bytes, then the
# first four bytes of a double SHA-256.
# --------------------------------------------------------------------------

ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
_ALPHABET_INDEX = {char: index for index, char in enumerate(ALPHABET)}
TRON_PREFIX = 0x41
ADDRESS_BYTES = 21


class AddressError(ValueError):
    """A value that is not a TRON address."""


def b58encode(data: bytes) -> str:
    number = int.from_bytes(data, "big")
    encoded = ""
    while number > 0:
        number, remainder = divmod(number, 58)
        encoded = ALPHABET[remainder] + encoded
    leading_zeros = len(data) - len(data.lstrip(b"\x00"))
    return "1" * leading_zeros + encoded


def b58decode(text: str) -> bytes:
    number = 0
    for char in text:
        if char not in _ALPHABET_INDEX:
            raise AddressError(f"not base58: {char!r}")
        number = number * 58 + _ALPHABET_INDEX[char]
    leading_ones = len(text) - len(text.lstrip("1"))
    body = number.to_bytes((number.bit_length() + 7) // 8, "big") if number else b""
    return b"\x00" * leading_ones + body


def _checksum(payload: bytes) -> bytes:
    return hashlib.sha256(hashlib.sha256(payload).digest()).digest()[:4]


def to_base58(address: bytes) -> str:
    """Encodes 21 canonical address bytes as a ``T...`` address."""
    if len(address) != ADDRESS_BYTES or address[0] != TRON_PREFIX:
        raise AddressError("a TRON address is 0x41 followed by 20 bytes")
    return b58encode(address + _checksum(address))


def from_base58(text: str) -> bytes:
    """Decodes a ``T...`` address into its 21 canonical bytes, checksum verified."""
    if not isinstance(text, str) or not text:
        raise AddressError("an address is a non-empty string")
    raw = b58decode(text.strip())
    if len(raw) != ADDRESS_BYTES + 4:
        raise AddressError("a TRON address decodes to 25 bytes")
    payload, checksum = raw[:ADDRESS_BYTES], raw[ADDRESS_BYTES:]
    if not hmac.compare_digest(_checksum(payload), checksum):
        raise AddressError("the address checksum does not match")
    if payload[0] != TRON_PREFIX:
        raise AddressError("a TRON address starts with 0x41")
    return payload


def derived_address(seed: str, label: str) -> bytes:
    """A deterministic address nobody holds a key for."""
    return bytes([TRON_PREFIX]) + hashlib.sha256(f"{seed}:address:{label}".encode()).digest()[:20]


# --------------------------------------------------------------------------
# The TRC20 Transfer event log, encoded the way a node's HTTP API returns it:
# the emitting contract without its 0x41 prefix, the event signature, both
# addresses as zero-padded 32-byte words, and the amount as one word.
# --------------------------------------------------------------------------

# keccak256("Transfer(address,address,uint256)")
TRANSFER_TOPIC = "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
MAX_UINT256 = (1 << 256) - 1

# Real token contracts the simulator refuses to mint: a transfer "of mainnet
# USDT" coming out of a simulator is exactly the reading that must never reach
# a deployment that believes it reads a real chain.
REAL_TOKEN_CONTRACTS = {
    "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t": "USDT on TRON mainnet",
    "TXYZopYRdj2D9XRtbG411XZZ3kM5VkAeBf": "USDT on the Nile testnet",
}


def _address_word(address: bytes) -> str:
    return "00" * 12 + address[1:].hex()


def encode_transfer_log(contract: bytes, sender: bytes, recipient: bytes, amount: int) -> dict[str, Any]:
    if not 0 < amount <= MAX_UINT256:
        raise ValueError("a transfer amount is a positive 256-bit integer")
    return {
        "address": contract[1:].hex(),
        "topics": [TRANSFER_TOPIC, _address_word(sender), _address_word(recipient)],
        "data": f"{amount:064x}",
    }


@dataclass(frozen=True)
class DecodedTransfer:
    event_index: int
    contract: bytes
    sender: bytes
    recipient: bytes
    amount: int


def decode_transfer_logs(logs: list[dict[str, Any]]) -> list[DecodedTransfer]:
    """Mirrors the gateway's decoder, so a test can prove the encoding round trips."""
    transfers = []
    for index, entry in enumerate(logs):
        topics = entry.get("topics") or []
        if not topics or topics[0].lower() != TRANSFER_TOPIC or len(topics) != 3:
            continue
        words = [bytes.fromhex(topic) for topic in topics[1:]]
        for word in words:
            if len(word) != 32 or any(word[:12]):
                raise ValueError("an address word is 32 bytes with 12 zero bytes of padding")
        data = bytes.fromhex(entry["data"])
        if len(data) != 32:
            raise ValueError("an amount word is 32 bytes")
        contract_hex = entry["address"].lower().removeprefix("0x")
        if len(contract_hex) == 40:
            contract_hex = "41" + contract_hex
        transfers.append(
            DecodedTransfer(
                event_index=index,
                contract=bytes.fromhex(contract_hex),
                sender=bytes([TRON_PREFIX]) + words[0][12:],
                recipient=bytes([TRON_PREFIX]) + words[1][12:],
                amount=int.from_bytes(data, "big"),
            )
        )
    return transfers


def parse_amount(value: Any) -> int:
    """A token amount is a decimal string of a positive 256-bit integer; never a float."""
    if not isinstance(value, str) or not value.isascii() or not value.isdigit():
        raise ValueError("amount_raw is a decimal string of digits, for example \"4999000\"")
    amount = int(value)
    if not 0 < amount <= MAX_UINT256:
        raise ValueError("amount_raw is a positive 256-bit integer")
    return amount


# --------------------------------------------------------------------------
# The chain.
# --------------------------------------------------------------------------


@dataclass
class SimulatedTransaction:
    tx_id: str
    block_number: int
    contract: bytes
    sender: bytes
    recipient: bytes
    amount: int
    success: bool
    sequence: int


@dataclass
class ChainConfig:
    seed: str = "chain-simulator"
    block_interval_ms: int = 3_000
    solid_lag: int = 19
    start_height: int = 100
    genesis_ms: int | None = None


class Chain:
    """A deterministic chain whose head is a function of the clock."""

    def __init__(self, config: ChainConfig, clock_ms: Callable[[], int] | None = None) -> None:
        if config.block_interval_ms <= 0:
            raise ValueError("the block interval is positive")
        if config.solid_lag < 0 or config.start_height < 0:
            raise ValueError("the solid lag and start height are not negative")
        self.config = config
        self._clock_ms = clock_ms or (lambda: time.time_ns() // 1_000_000)
        now = self._clock_ms()
        self.genesis_ms = (
            config.genesis_ms
            if config.genesis_ms is not None
            else now - config.start_height * config.block_interval_ms
        )
        self._lock = threading.Lock()
        self._last_now = now
        self._by_block: dict[int, list[SimulatedTransaction]] = {}
        self._by_id: dict[str, SimulatedTransaction] = {}
        self._sequence = 0
        self.default_sender = derived_address(config.seed, "payer")
        self.default_contract = derived_address(config.seed, "usdt")

    # The clock never runs backwards inside the simulator, so a block that was
    # visible once stays visible and its contents stay frozen.
    def _now(self) -> int:
        self._last_now = max(self._last_now, self._clock_ms())
        return self._last_now

    def _head_at(self, now: int) -> int:
        return (now - self.genesis_ms) // self.config.block_interval_ms

    def heads(self) -> tuple[int, int]:
        with self._lock:
            head = self._head_at(self._now())
        return head, max(head - self.config.solid_lag, -1)

    def block_id(self, number: int) -> str:
        digest = hashlib.sha256(f"{self.config.seed}:block:{number}".encode()).hexdigest()
        return f"{number:016x}{digest[16:]}"

    def block_timestamp(self, number: int) -> int:
        return self.genesis_ms + number * self.config.block_interval_ms

    def block(self, number: int, solid_only: bool = False) -> dict[str, Any]:
        head, solid = self.heads()
        limit = solid if solid_only else head
        if number < 0 or number > limit:
            return {}
        return {
            "blockID": self.block_id(number),
            "block_header": {
                "raw_data": {
                    "number": number,
                    "timestamp": self.block_timestamp(number),
                    "parentHash": self.block_id(number - 1) if number > 0 else "0" * 64,
                    "witness_address": derived_address(self.config.seed, "witness").hex(),
                    "version": 30,
                },
                "witness_signature": "00" * 65,
            },
        }

    def submit(
        self,
        recipient: bytes,
        amount: int,
        contract: bytes | None = None,
        sender: bytes | None = None,
        success: bool = True,
    ) -> SimulatedTransaction:
        contract = contract or self.default_contract
        sender = sender or self.default_sender
        with self._lock:
            number = self._head_at(self._now()) + 1
            self._sequence += 1
            material = (
                f"{self.config.seed}:tx:{self._sequence}:{number}:{contract.hex()}:"
                f"{sender.hex()}:{recipient.hex()}:{amount}:{success}"
            )
            tx = SimulatedTransaction(
                tx_id=hashlib.sha256(material.encode()).hexdigest(),
                block_number=number,
                contract=contract,
                sender=sender,
                recipient=recipient,
                amount=amount,
                success=success,
                sequence=self._sequence,
            )
            self._by_block.setdefault(number, []).append(tx)
            self._by_id[tx.tx_id] = tx
        return tx

    def _info(self, tx: SimulatedTransaction) -> dict[str, Any]:
        info: dict[str, Any] = {
            "id": tx.tx_id,
            "fee": 0,
            "blockNumber": tx.block_number,
            "blockTimeStamp": self.block_timestamp(tx.block_number),
            "contractResult": [""],
            "contract_address": tx.contract.hex(),
            "receipt": {"energy_usage_total": 0, "net_usage": 0,
                        "result": "SUCCESS" if tx.success else "REVERT"},
            # A reverted transaction on a real node carries no log. The
            # simulator keeps it so the gateway's refusal of a failed
            # execution is exercised instead of never being reached.
            "log": [encode_transfer_log(tx.contract, tx.sender, tx.recipient, tx.amount)],
        }
        if not tx.success:
            info["result"] = "FAILED"
            info["resMessage"] = "REVERT opcode executed".encode().hex()
        return info

    def transaction_info(self, tx_id: str, solid_only: bool = False) -> dict[str, Any]:
        head, solid = self.heads()
        tx = self._by_id.get(tx_id.lower().removeprefix("0x"))
        if tx is None or tx.block_number > (solid if solid_only else head):
            return {}
        return self._info(tx)

    def block_transaction_infos(self, number: int, solid_only: bool = False) -> list[dict[str, Any]]:
        head, solid = self.heads()
        if number < 0 or number > (solid if solid_only else head):
            return []
        return [self._info(tx) for tx in self._by_block.get(number, [])]

    def trc20_index(self, address: bytes, min_timestamp: int, limit: int) -> list[dict[str, Any]]:
        """Successful transfers to ``address`` in produced blocks, oldest first."""
        head, _ = self.heads()
        with self._lock:
            candidates = sorted(self._by_id.values(), key=lambda tx: (tx.block_number, tx.sequence))
        rows = []
        for tx in candidates:
            timestamp = self.block_timestamp(tx.block_number)
            if (not tx.success or tx.recipient != address or tx.block_number > head
                    or timestamp < min_timestamp):
                continue
            rows.append({
                "transaction_id": tx.tx_id,
                "block_timestamp": timestamp,
                "from": to_base58(tx.sender),
                "to": to_base58(tx.recipient),
                "type": "Transfer",
                "value": str(tx.amount),
                "token_info": {"address": to_base58(tx.contract)},
            })
            if len(rows) >= limit:
                break
        return rows

    def state(self) -> dict[str, Any]:
        head, solid = self.heads()
        with self._lock:
            transactions = [
                {
                    "tx_id": tx.tx_id,
                    "block_number": tx.block_number,
                    "status": "success" if tx.success else "failed",
                    "to": to_base58(tx.recipient),
                    "from": to_base58(tx.sender),
                    "contract": to_base58(tx.contract),
                    "amount_raw": str(tx.amount),
                    "visible": tx.block_number <= head,
                    "solidified": tx.block_number <= solid,
                }
                for tx in sorted(self._by_id.values(), key=lambda tx: tx.sequence)
            ]
        return {
            "simulator": True,
            "warning": "simulated chain: proves nothing about any real blockchain",
            "seed": self.config.seed,
            "head": head,
            "solid_head": solid,
            "solid_lag": self.config.solid_lag,
            "block_interval_ms": self.config.block_interval_ms,
            "genesis_ms": self.genesis_ms,
            "default_contract": to_base58(self.default_contract),
            "default_sender": to_base58(self.default_sender),
            "transactions": transactions,
        }


# --------------------------------------------------------------------------
# Access to the control API.
# --------------------------------------------------------------------------

TOKEN_HEADER = "X-Simulator-Token"


def control_allowed(client_host: str, presented_token: str | None, configured_token: str | None) -> bool:
    """Loopback callers are served; anyone else must present the configured token."""
    try:
        address = ipaddress.ip_address(client_host)
    except ValueError:
        address = None
    if address is not None:
        mapped = getattr(address, "ipv4_mapped", None)
        if address.is_loopback or (mapped is not None and mapped.is_loopback):
            return True
    if not configured_token or not presented_token:
        return False
    return hmac.compare_digest(presented_token.encode(), configured_token.encode())


# --------------------------------------------------------------------------
# HTTP.
# --------------------------------------------------------------------------

MAX_BODY_BYTES = 64 * 1024


class RequestError(Exception):
    def __init__(self, status: int, message: str) -> None:
        super().__init__(message)
        self.status = status


def _address_argument(body: dict[str, Any], name: str, required: bool) -> bytes | None:
    value = body.get(name)
    if value is None:
        if required:
            raise RequestError(400, f"{name} is required")
        return None
    try:
        return from_base58(value)
    except AddressError as error:
        raise RequestError(400, f"{name} is not a TRON address: {error}") from error


def make_handler(chain: Chain, control_token: str | None) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        server_version = "chain-simulator/1"
        protocol_version = "HTTP/1.1"

        def log_message(self, format: str, *args: Any) -> None:  # noqa: A002
            log.info("%s %s", self.client_address[0], format % args)

        def _send(self, status: int, payload: Any) -> None:
            body = json.dumps(payload).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def _body(self) -> dict[str, Any]:
            try:
                length = int(self.headers.get("Content-Length") or "0")
            except ValueError as error:
                raise RequestError(411, "Content-Length is required") from error
            if length < 0 or length > MAX_BODY_BYTES:
                raise RequestError(413, "body too large")
            raw = self.rfile.read(length) if length else b""
            if not raw.strip():
                return {}
            try:
                value = json.loads(raw.decode("utf-8"), parse_float=_refuse_float)
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                raise RequestError(400, "the body is not JSON") from error
            except ValueError as error:
                raise RequestError(400, str(error)) from error
            if not isinstance(value, dict):
                raise RequestError(400, "the body is a JSON object")
            return value

        def _guard_control(self) -> None:
            if not control_allowed(self.client_address[0], self.headers.get(TOKEN_HEADER), control_token):
                raise RequestError(403, "the control API answers loopback callers or the configured token")

        def _handle(self, method: str) -> None:
            try:
                status, payload = self._route(method)
            except RequestError as error:
                status, payload = error.status, {"error": str(error)}
            self._send(status, payload)

        def do_GET(self) -> None:  # noqa: N802
            self._handle("GET")

        def do_POST(self) -> None:  # noqa: N802
            self._handle("POST")

        def _route(self, method: str) -> tuple[int, Any]:
            parts = urlsplit(self.path)
            path = parts.path.rstrip("/")
            if path.startswith("/simulator"):
                self._guard_control()
                if method == "GET" and path == "/simulator/state":
                    return 200, chain.state()
                if method == "POST" and path == "/simulator/transfers":
                    return 200, self._transfer(self._body())
                raise RequestError(404, "unknown control route")

            if method == "POST":
                body = self._body()
                solid_only = path.startswith("/walletsolidity/")
                name = path.rsplit("/", 1)[-1]
                if path in ("/wallet/getnowblock", "/walletsolidity/getnowblock"):
                    head, solid = chain.heads()
                    return 200, chain.block(solid if solid_only else head, solid_only)
                if name == "getblockbynum" and path.startswith(("/wallet/", "/walletsolidity/")):
                    return 200, chain.block(_number(body), solid_only)
                if name == "gettransactioninfobyblocknum" and path.startswith(("/wallet/", "/walletsolidity/")):
                    return 200, chain.block_transaction_infos(_number(body), solid_only)
                if name == "gettransactioninfobyid" and path.startswith(("/wallet/", "/walletsolidity/")):
                    value = body.get("value")
                    if not isinstance(value, str):
                        raise RequestError(400, "value is the transaction id")
                    return 200, chain.transaction_info(value, solid_only)

            if method == "GET" and path.startswith("/v1/accounts/") and path.endswith("/transactions/trc20"):
                address_text = path[len("/v1/accounts/"):-len("/transactions/trc20")]
                try:
                    address = from_base58(address_text)
                except AddressError as error:
                    raise RequestError(400, f"not a TRON address: {error}") from error
                query = parse_qs(parts.query)
                limit = _query_int(query, "limit", 20, 1, 200)
                min_timestamp = _query_int(query, "min_timestamp", 0, 0, 1 << 62)
                rows = chain.trc20_index(address, min_timestamp, limit)
                return 200, {"data": rows, "success": True,
                             "meta": {"at": chain.block_timestamp(chain.heads()[0]), "page_size": len(rows)}}

            raise RequestError(404, "the simulator does not serve this route")

        def _transfer(self, body: dict[str, Any]) -> dict[str, Any]:
            recipient = _address_argument(body, "to", required=True)
            contract = _address_argument(body, "contract", required=False)
            sender = _address_argument(body, "from", required=False)
            if contract is not None and to_base58(contract) in REAL_TOKEN_CONTRACTS:
                raise RequestError(
                    400,
                    f"refused: {REAL_TOKEN_CONTRACTS[to_base58(contract)]} is a real token; "
                    "the simulator mints only simulated contracts",
                )
            try:
                amount = parse_amount(body.get("amount_raw"))
            except ValueError as error:
                raise RequestError(400, str(error)) from error
            status = body.get("status", "success")
            if status not in ("success", "failed"):
                raise RequestError(400, "status is success or failed")
            assert recipient is not None
            tx = chain.submit(recipient, amount, contract, sender, success=status == "success")
            log.info("mined tx=%s block=%d to=%s amount_raw=%s status=%s",
                     tx.tx_id, tx.block_number, to_base58(recipient), amount, status)
            return {
                "tx_id": tx.tx_id,
                "block_number": tx.block_number,
                "block_timestamp": chain.block_timestamp(tx.block_number),
                "event_index": 0,
                "status": status,
                "to": to_base58(tx.recipient),
                "from": to_base58(tx.sender),
                "contract": to_base58(tx.contract),
                "amount_raw": str(tx.amount),
                "solidified_at_block": tx.block_number + chain.config.solid_lag,
            }

    return Handler


def _refuse_float(value: str) -> Any:
    raise ValueError("numbers with a fraction are refused; send amounts as decimal strings")


def _number(body: dict[str, Any]) -> int:
    value = body.get("num")
    if not isinstance(value, int) or isinstance(value, bool):
        raise RequestError(400, "num is the block number")
    return value


def _query_int(query: dict[str, list[str]], name: str, default: int, low: int, high: int) -> int:
    values = query.get(name)
    if not values:
        return default
    try:
        value = int(values[0])
    except ValueError as error:
        raise RequestError(400, f"{name} is an integer") from error
    return min(max(value, low), high)


def build_server(host: str, port: int, chain: Chain, control_token: str | None) -> ThreadingHTTPServer:
    server = ThreadingHTTPServer((host, port), make_handler(chain, control_token))
    server.daemon_threads = True
    return server


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(
        description="A simulated TRON node for integration tests. Never point a mainnet deployment at it.")
    env = os.environ.get
    parser.add_argument("--host", default=env("SIMULATOR_HOST", "127.0.0.1"))
    parser.add_argument("--port", type=int, default=int(env("SIMULATOR_PORT", "8090")))
    parser.add_argument("--block-interval-seconds", type=float,
                        default=float(env("SIMULATOR_BLOCK_INTERVAL_SECONDS", "3")))
    parser.add_argument("--solid-lag", type=int, default=int(env("SIMULATOR_SOLID_LAG", "19")))
    parser.add_argument("--start-height", type=int, default=int(env("SIMULATOR_START_HEIGHT", "100")))
    parser.add_argument("--seed", default=env("SIMULATOR_SEED", "chain-simulator"))
    args = parser.parse_args(argv)

    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(message)s", stream=sys.stderr)
    token = env("SIMULATOR_CONTROL_TOKEN") or None
    interval_ms = int(round(args.block_interval_seconds * 1000))
    chain = Chain(ChainConfig(seed=args.seed, block_interval_ms=interval_ms,
                              solid_lag=args.solid_lag, start_height=args.start_height))
    server = build_server(args.host, args.port, chain, token)
    log.warning("SIMULATED TRON CHAIN - testing only; it proves nothing about any real chain")
    log.info("listening on %s:%d block_interval_ms=%d solid_lag=%d default_contract=%s control_token=%s",
             args.host, args.port, interval_ms, args.solid_lag, to_base58(chain.default_contract),
             "configured" if token else "none (loopback only)")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
