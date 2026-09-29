import { describe, expect, it } from "vitest";
import { assertMinor, formatAmount, formatExpiry, formatSigned, parseAmount, parseLots } from "./format";

describe("formatAmount", () => {
  it("formats minor units with two decimals", () => {
    expect(formatAmount(0)).toBe("0.00");
    expect(formatAmount(5)).toBe("0.05");
    expect(formatAmount(120)).toBe("1.20");
    expect(formatAmount(4_800)).toBe("48.00");
    expect(formatAmount(123_456)).toBe("1,234.56");
    expect(formatAmount(100_000_000)).toBe("1,000,000.00");
  });

  it("rejects values that are not safe non-negative integers", () => {
    expect(() => formatAmount(1.5)).toThrow(RangeError);
    expect(() => formatAmount(-1)).toThrow(RangeError);
    expect(() => formatAmount(2 ** 53)).toThrow(RangeError);
    expect(() => assertMinor(Number.NaN)).toThrow(RangeError);
  });

  it("formats a crossed spread as negative", () => {
    expect(formatSigned(-5)).toBe("-0.05");
    expect(formatSigned(3)).toBe("0.03");
  });
});

describe("parseAmount", () => {
  it("parses up to two decimals without floats", () => {
    expect(parseAmount("1.20")).toBe(120);
    expect(parseAmount("1.2")).toBe(120);
    expect(parseAmount("1.")).toBe(100);
    expect(parseAmount("0.07")).toBe(7);
    expect(parseAmount(" 48 ")).toBe(4_800);
    expect(parseAmount("1,000.50")).toBe(100_050);
    // 0.1 + 0.2 style float error would give 29 or 30.000000004
    expect(parseAmount("0.29")).toBe(29);
  });

  it("rejects anything else", () => {
    for (const bad of ["", ".5", "1.234", "-1", "1e3", "abc", "1.2.3", "NaN"]) {
      expect(parseAmount(bad), bad).toBeNull();
    }
  });

  it("round-trips with formatAmount", () => {
    for (const minor of [0, 1, 99, 100, 120, 4_800, 123_456]) {
      expect(parseAmount(formatAmount(minor))).toBe(minor);
    }
  });
});

describe("parseLots", () => {
  it("accepts positive whole numbers only", () => {
    expect(parseLots("40")).toBe(40);
    for (const bad of ["0", "-1", "1.5", "", "x"]) {
      expect(parseLots(bad), bad).toBeNull();
    }
  });
});

describe("formatExpiry", () => {
  it("counts down in the largest sensible unit", () => {
    const now = 1_000_000_000_000;
    expect(formatExpiry(now + 30 * 1e6, now)).toBe("in 30 s");
    expect(formatExpiry(now + 15 * 60 * 1e6, now)).toBe("in 15 min");
    expect(formatExpiry(now + 3 * 3600 * 1e6, now)).toBe("in 3 h");
    expect(formatExpiry(now, now)).toBe("expired");
  });
});
