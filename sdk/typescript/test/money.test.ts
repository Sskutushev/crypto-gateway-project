import assert from "node:assert/strict";
import { describe, test } from "node:test";

import { formatTokenAmount, isIntegerString, parseMinorUnits } from "../src/index.js";

describe("formatTokenAmount", () => {
  test("USDT's 6 decimals, exactly", () => {
    assert.equal(formatTokenAmount("4999000", 6), "4.999");
    assert.equal(formatTokenAmount("49990000", 6), "49.99");
    assert.equal(formatTokenAmount("1", 6), "0.000001");
    assert.equal(formatTokenAmount("1000000", 6), "1");
    assert.equal(formatTokenAmount("0", 6), "0");
    assert.equal(formatTokenAmount("000123", 6), "0.000123");
  });

  test("0 decimals is the integer itself, without leading zeros", () => {
    assert.equal(formatTokenAmount("0", 0), "0");
    assert.equal(formatTokenAmount("00042", 0), "42");
    assert.equal(formatTokenAmount("123456789012345678901234567890", 0), "123456789012345678901234567890");
  });

  test("18 decimals", () => {
    assert.equal(formatTokenAmount("1", 18), "0.000000000000000001");
    assert.equal(formatTokenAmount("1000000000000000000", 18), "1");
    assert.equal(formatTokenAmount("1234567890123456789", 18), "1.234567890123456789");
    assert.equal(formatTokenAmount("1500000000000000000", 18), "1.5");
  });

  test("values beyond 2^53 lose nothing", () => {
    // 2^53 + 1 is the first integer a double cannot hold.
    assert.equal(formatTokenAmount("9007199254740993", 6), "9007199254.740993");
    assert.equal(formatTokenAmount("9007199254740993", 0), "9007199254740993");
    const uint256Max = "115792089237316195423570985008687907853269984665640564039457584007913129639935";
    assert.equal(
      formatTokenAmount(uint256Max, 77),
      "1.15792089237316195423570985008687907853269984665640564039457584007913129639935",
    );
    assert.equal(formatTokenAmount(uint256Max, 18), "115792089237316195423570985008687907853269984665640564039457.584007913129639935");
  });

  test("refuses what is not an integer string or a valid scale", () => {
    for (const raw of ["", "-1", "1.5", "1e6", " 1", "0x10", "\u0661"]) {
      assert.throws(() => formatTokenAmount(raw, 6), TypeError, raw);
    }
    assert.throws(() => formatTokenAmount(4999000 as unknown as string, 6), TypeError);
    for (const decimals of [-1, 78, 1.5, Number.NaN]) {
      assert.throws(() => formatTokenAmount("1", decimals), RangeError, String(decimals));
    }
  });
});

describe("parseMinorUnits", () => {
  test("converts a display amount into minor units", () => {
    assert.equal(parseMinorUnits("49.99", 2), "4999");
    assert.equal(parseMinorUnits("49.9", 2), "4990");
    assert.equal(parseMinorUnits("49", 2), "4900");
    assert.equal(parseMinorUnits("0.01", 2), "1");
    assert.equal(parseMinorUnits("0", 2), "0");
    assert.equal(parseMinorUnits("007.50", 2), "750");
    assert.equal(parseMinorUnits("4.999", 6), "4999000");
    assert.equal(parseMinorUnits("12", 0), "12");
  });

  test("never rounds: too many fractional digits is an error", () => {
    assert.throws(() => parseMinorUnits("49.999", 2), RangeError);
    assert.throws(() => parseMinorUnits("1.0", 0), RangeError);
  });

  test("refuses signs, exponents, separators and space", () => {
    for (const amount of ["", ".5", "5.", "-1", "+1", "1e3", "1,000.00", " 1", "1 ", "NaN", "Infinity", "0x1"]) {
      assert.throws(() => parseMinorUnits(amount, 2), TypeError, JSON.stringify(amount));
    }
  });

  test("values beyond 2^53 lose nothing", () => {
    assert.equal(parseMinorUnits("9007199254.740993", 6), "9007199254740993");
    assert.equal(parseMinorUnits("1.234567890123456789", 18), "1234567890123456789");
  });

  test("is the inverse of formatTokenAmount", () => {
    let lcg = 42;
    const next = () => {
      lcg = (lcg * 48271) % 2147483647;
      return lcg;
    };
    for (let i = 0; i < 500; i += 1) {
      const decimals = next() % 25;
      const digits = 1 + (next() % 40);
      let raw = "";
      for (let d = 0; d < digits; d += 1) {
        raw += String(next() % 10);
      }
      const normalised = BigInt(raw).toString();
      assert.equal(parseMinorUnits(formatTokenAmount(raw, decimals), decimals), normalised, `${raw}/${decimals}`);
    }
  });
});

test("isIntegerString", () => {
  assert.equal(isIntegerString("0"), true);
  assert.equal(isIntegerString("123"), true);
  assert.equal(isIntegerString(""), false);
  assert.equal(isIntegerString(123), false);
  assert.equal(isIntegerString("1.0"), false);
});
