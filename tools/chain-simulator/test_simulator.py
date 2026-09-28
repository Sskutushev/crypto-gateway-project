"""Unit tests for the chain simulator. Run: python3 -m unittest -v"""

from __future__ import annotations

import json
import threading
import unittest
import urllib.error
import urllib.request
from pathlib import Path

import simulator as sim

FIXTURES = Path(__file__).resolve().parents[2] / "crates" / "gateway-tron" / "fixtures"


class FakeClock:
    def __init__(self, now_ms: int) -> None:
        self.now_ms = now_ms

    def __call__(self) -> int:
        return self.now_ms

    def advance_blocks(self, chain: sim.Chain, blocks: int) -> None:
        self.now_ms += blocks * chain.config.block_interval_ms


def fixed_chain(**overrides: int) -> tuple[sim.Chain, FakeClock]:
    clock = FakeClock(1_758_000_000_000)
    config = sim.ChainConfig(block_interval_ms=3_000, solid_lag=19, start_height=100)
    for name, value in overrides.items():
        setattr(config, name, value)
    return sim.Chain(config, clock), clock


class Base58Test(unittest.TestCase):
    # Known TRON addresses and their canonical bytes: the mainnet USDT
    # contract, the Nile faucet USDT, and the placeholder collector the dev
    # rail seeds (0x41 followed by twenty 0x03 bytes).
    KNOWN = {
        "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t": "41a614f803b6fd780986a42c78ec9c7f77e6ded13c",
        "TXYZopYRdj2D9XRtbG411XZZ3kM5VkAeBf": "41eca9bc828a3005b9a3b909f2cc5c2a54794de05f",
        "TAF8dttxK5iPKbvYC626aDBytrWANpLRXp": "41" + "03" * 20,
    }

    def test_known_addresses_round_trip(self) -> None:
        for text, hex_bytes in self.KNOWN.items():
            with self.subTest(address=text):
                self.assertEqual(sim.from_base58(text).hex(), hex_bytes)
                self.assertEqual(sim.to_base58(bytes.fromhex(hex_bytes)), text)

    def test_a_changed_character_fails_the_checksum(self) -> None:
        with self.assertRaises(sim.AddressError):
            sim.from_base58("TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6u")

    def test_malformed_values_are_refused(self) -> None:
        for bad in ("", "0OIl", "T", "not-an-address"):
            with self.subTest(value=bad), self.assertRaises(sim.AddressError):
                sim.from_base58(bad)
        with self.assertRaises(sim.AddressError):
            sim.to_base58(bytes.fromhex("42" + "03" * 20))

    def test_leading_zero_bytes_survive(self) -> None:
        self.assertEqual(sim.b58decode(sim.b58encode(b"\x00\x00\x01")), b"\x00\x00\x01")


class TransferLogTest(unittest.TestCase):
    def test_an_encoded_log_decodes_to_the_same_transfer(self) -> None:
        contract = sim.derived_address("t", "contract")
        sender = sim.derived_address("t", "from")
        recipient = sim.derived_address("t", "to")
        for amount in (1, 4_999_000, sim.MAX_UINT256):
            with self.subTest(amount=amount):
                log = sim.encode_transfer_log(contract, sender, recipient, amount)
                self.assertEqual(log["topics"][0], sim.TRANSFER_TOPIC)
                self.assertEqual(len(log["address"]), 40, "a node reports the contract without 0x41")
                [decoded] = sim.decode_transfer_logs([log])
                self.assertEqual(decoded.contract, contract)
                self.assertEqual(decoded.sender, sender)
                self.assertEqual(decoded.recipient, recipient)
                self.assertEqual(decoded.amount, amount)
                self.assertEqual(decoded.event_index, 0)

    def test_the_encoding_matches_the_gateway_fixture(self) -> None:
        fixture = json.loads((FIXTURES / "transfer_success.json").read_text(encoding="utf-8"))
        transfer_log = fixture["log"][1]
        [decoded] = sim.decode_transfer_logs([transfer_log])
        rebuilt = sim.encode_transfer_log(decoded.contract, decoded.sender, decoded.recipient, decoded.amount)
        self.assertEqual(rebuilt, {key: transfer_log[key] for key in ("address", "topics", "data")})

    def test_zero_and_oversized_amounts_are_refused(self) -> None:
        address = sim.derived_address("t", "x")
        for amount in (0, -1, sim.MAX_UINT256 + 1):
            with self.subTest(amount=amount), self.assertRaises(ValueError):
                sim.encode_transfer_log(address, address, address, amount)

    def test_amounts_are_decimal_strings_only(self) -> None:
        self.assertEqual(sim.parse_amount("4999000"), 4_999_000)
        for bad in (4999000, 4999.0, "4999.0", "-1", "0", "1e6", " 1", "", None, "١٢"):
            with self.subTest(value=bad), self.assertRaises(ValueError):
                sim.parse_amount(bad)


class ChainTest(unittest.TestCase):
    def test_the_solid_head_trails_the_head_by_the_configured_lag(self) -> None:
        chain, clock = fixed_chain()
        head, solid = chain.heads()
        self.assertEqual(head, 100)
        self.assertEqual(solid, 81)
        clock.advance_blocks(chain, 5)
        self.assertEqual(chain.heads(), (105, 86))

    def test_blocks_are_consistent_across_endpoints(self) -> None:
        chain, _ = fixed_chain()
        head, solid = chain.heads()
        now_block = chain.block(head)
        by_number = chain.block(head)
        self.assertEqual(now_block, by_number)
        raw = now_block["block_header"]["raw_data"]
        self.assertEqual(raw["number"], head)
        self.assertEqual(raw["parentHash"], chain.block(head - 1)["blockID"])
        self.assertEqual(raw["timestamp"] - chain.block(head - 1)["block_header"]["raw_data"]["timestamp"], 3_000)
        self.assertTrue(now_block["blockID"].startswith(f"{head:016x}"))
        self.assertEqual(len(now_block["blockID"]), 64)
        self.assertEqual(chain.block(head + 1), {}, "a future block does not exist yet")
        self.assertEqual(chain.block(head, solid_only=True), {}, "the solidity API stops at the solid head")
        self.assertEqual(chain.block(solid, solid_only=True)["blockID"], chain.block_id(solid))

    def test_a_transfer_is_mined_into_the_next_block_and_frozen_before_it_is_visible(self) -> None:
        chain, clock = fixed_chain()
        recipient = sim.derived_address("t", "collector")
        tx = chain.submit(recipient, 4_999_000)
        self.assertEqual(tx.block_number, 101)
        self.assertEqual(chain.transaction_info(tx.tx_id), {}, "not produced yet")
        self.assertEqual(chain.block_transaction_infos(101), [])
        clock.advance_blocks(chain, 1)
        info = chain.transaction_info(tx.tx_id)
        self.assertEqual(info["blockNumber"], 101)
        self.assertEqual(info["receipt"]["result"], "SUCCESS")
        self.assertEqual(info["blockTimeStamp"], chain.block(101)["block_header"]["raw_data"]["timestamp"])
        self.assertEqual(chain.block_transaction_infos(101), [info])
        [decoded] = sim.decode_transfer_logs(info["log"])
        self.assertEqual((decoded.recipient, decoded.amount), (recipient, 4_999_000))
        self.assertEqual(chain.transaction_info(tx.tx_id, solid_only=True), {})
        clock.advance_blocks(chain, 19)
        self.assertEqual(chain.transaction_info(tx.tx_id, solid_only=True), info)

    def test_a_failed_transfer_reports_a_failed_execution(self) -> None:
        chain, clock = fixed_chain()
        tx = chain.submit(sim.derived_address("t", "c"), 5, success=False)
        clock.advance_blocks(chain, 1)
        info = chain.transaction_info(tx.tx_id)
        self.assertEqual(info["result"], "FAILED")
        self.assertEqual(info["receipt"]["result"], "REVERT")

    def test_the_address_index_lists_successful_visible_transfers_only(self) -> None:
        chain, clock = fixed_chain()
        collector = sim.derived_address("t", "collector")
        paid = chain.submit(collector, 10)
        chain.submit(collector, 11, success=False)
        chain.submit(sim.derived_address("t", "someone-else"), 12)
        self.assertEqual(chain.trc20_index(collector, 0, 50), [], "nothing is produced yet")
        clock.advance_blocks(chain, 1)
        rows = chain.trc20_index(collector, 0, 50)
        self.assertEqual([row["transaction_id"] for row in rows], [paid.tx_id])
        self.assertEqual(rows[0]["value"], "10")
        after = rows[0]["block_timestamp"] + 1
        self.assertEqual(chain.trc20_index(collector, after, 50), [])

    def test_the_same_seed_produces_the_same_chain(self) -> None:
        first, _ = fixed_chain()
        second, _ = fixed_chain()
        self.assertEqual(first.block(50), second.block(50))
        recipient = sim.derived_address("t", "r")
        self.assertEqual(first.submit(recipient, 7).tx_id, second.submit(recipient, 7).tx_id)


class ControlAccessTest(unittest.TestCase):
    def test_loopback_callers_are_served(self) -> None:
        for host in ("127.0.0.1", "127.8.9.10", "::1", "::ffff:127.0.0.1"):
            with self.subTest(host=host):
                self.assertTrue(sim.control_allowed(host, None, None))

    def test_non_local_callers_without_the_token_are_refused(self) -> None:
        for host in ("10.0.0.5", "172.18.0.4", "192.168.1.2", "8.8.8.8", "not-an-ip"):
            with self.subTest(host=host):
                self.assertFalse(sim.control_allowed(host, None, None))
                self.assertFalse(sim.control_allowed(host, None, "secret-token"))
                self.assertFalse(sim.control_allowed(host, "wrong", "secret-token"))
                self.assertFalse(sim.control_allowed(host, "anything", None))
                self.assertFalse(sim.control_allowed(host, "", ""))

    def test_non_local_callers_with_the_token_are_served(self) -> None:
        self.assertTrue(sim.control_allowed("172.18.0.4", "secret-token", "secret-token"))


class HttpTest(unittest.TestCase):
    """The server end to end over a real socket, on loopback."""

    def setUp(self) -> None:
        self.chain, self.clock = fixed_chain()
        self.server = sim.build_server("127.0.0.1", 0, self.chain, None)
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self) -> None:
        self.server.shutdown()
        self.server.server_close()

    def call(self, method: str, path: str, body: object | None = None, raw: bytes | None = None) -> tuple[int, object]:
        data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
        request = urllib.request.Request(self.base + path, data=data, method=method,
                                         headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=5) as response:
                return response.status, json.loads(response.read())
        except urllib.error.HTTPError as error:
            return error.code, json.loads(error.read())

    def test_the_gateway_call_sequence(self) -> None:
        collector = sim.to_base58(sim.derived_address("t", "collector"))
        status, mined = self.call("POST", "/simulator/transfers", {"to": collector, "amount_raw": "4999000"})
        self.assertEqual(status, 200)
        self.clock.advance_blocks(self.chain, 20)

        _, head = self.call("POST", "/wallet/getnowblock", {})
        _, solid = self.call("POST", "/walletsolidity/getnowblock", {})
        self.assertEqual(head["block_header"]["raw_data"]["number"] - solid["block_header"]["raw_data"]["number"], 19)
        _, block = self.call("POST", "/wallet/getblockbynum", {"num": mined["block_number"]})
        self.assertEqual(block["blockID"], self.chain.block_id(mined["block_number"]))
        _, infos = self.call("POST", "/wallet/gettransactioninfobyblocknum", {"num": mined["block_number"]})
        self.assertEqual([info["id"] for info in infos], [mined["tx_id"]])
        _, info = self.call("POST", "/wallet/gettransactioninfobyid", {"value": mined["tx_id"]})
        self.assertEqual(info["blockNumber"], mined["block_number"])
        _, unknown = self.call("POST", "/wallet/gettransactioninfobyid", {"value": "ab" * 32})
        self.assertEqual(unknown, {})
        _, page = self.call(
            "GET",
            f"/v1/accounts/{collector}/transactions/trc20?only_to=true&limit=50&min_timestamp=0"
            "&order_by=block_timestamp,asc")
        self.assertEqual([row["transaction_id"] for row in page["data"]], [mined["tx_id"]])

    def test_the_control_api_refuses_bad_input(self) -> None:
        collector = sim.to_base58(sim.derived_address("t", "collector"))
        cases = [
            {"to": collector, "amount_raw": 4999000},
            {"to": collector, "amount_raw": "0"},
            {"to": "Tnotanaddress", "amount_raw": "1"},
            {"amount_raw": "1"},
            {"to": collector, "amount_raw": "1", "status": "pending"},
            {"to": collector, "amount_raw": "1", "contract": "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t"},
        ]
        for body in cases:
            with self.subTest(body=body):
                status, answer = self.call("POST", "/simulator/transfers", body)
                self.assertEqual(status, 400, answer)
        status, _ = self.call("POST", "/simulator/transfers",
                              raw=b'{"to": "%s", "amount_raw": "1", "x": 1.5}' % collector.encode())
        self.assertEqual(status, 400)
        self.assertEqual(self.chain.state()["transactions"], [])

    def test_state_is_served_to_loopback(self) -> None:
        status, state = self.call("GET", "/simulator/state")
        self.assertEqual(status, 200)
        self.assertTrue(state["simulator"])
        self.assertEqual(state["head"] - state["solid_head"], 19)


if __name__ == "__main__":
    unittest.main()
