import { describe, expect, it } from "vitest";
import { assertMinor, formatAmount, formatAveragePrice, formatCountdown, formatExpiry, formatSigned, parseAmount, parseBps, parseLots } from "./format";

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

describe("formatAveragePrice", () => {
  it("shows an exact rational with integer rounding", () => {
    expect(formatAveragePrice(4_800, 40)).toBe("1.2000");
    expect(formatAveragePrice(4_800 + 2_420, 60)).toBe("1.2033"); // 120.333…
    expect(formatAveragePrice(1_790, 15)).toBe("1.1933"); // 119.333…
    expect(formatAveragePrice(2, 3)).toBe("0.0067"); // 0.6666… rounds half up
    expect(formatAveragePrice(1_200, 10, 2)).toBe("1.20");
    expect(formatAveragePrice(0, 0)).toBe("—");
  });
});

describe("parseBps", () => {
  it("reads a percentage as basis points", () => {
    expect(parseBps("2")).toBe(200);
    expect(parseBps("0.5")).toBe(50);
    expect(parseBps("0")).toBe(0);
    expect(parseBps("1.234")).toBeNull();
  });
});

describe("unit decimals", () => {
  it("formats and parses at a unit's declared decimals", () => {
    expect(formatAmount(1_234_567, 3)).toBe("1,234.567");
    expect(formatAmount(1_234, 0)).toBe("1,234");
    expect(formatAmount(5, 8)).toBe("0.00000005");
    expect(parseAmount("1.5", 3)).toBe(1_500);
    expect(parseAmount("1.2345", 3)).toBeNull();
    expect(parseAmount("12", 0)).toBe(12);
    expect(parseAmount("1.2", 0)).toBeNull();
    for (const [minor, d] of [[0, 0], [7, 3], [123_456, 4], [1, 8]] as const) {
      expect(parseAmount(formatAmount(minor, d), d)).toBe(minor);
    }
  });

  it("averages in the quote unit's decimals", () => {
    // 1.2033 HF at 2 decimals; the same exact price at 3 quote decimals.
    expect(formatAveragePrice(7_220, 60)).toBe("1.2033");
    expect(formatAveragePrice(72_200, 60, 4, 3)).toBe("1.2033");
  });
});

describe("formatCountdown", () => {
  const now = 1_000_000_000_000;
  it("counts down in m:ss and h:mm:ss, stopping at zero", () => {
    expect(formatCountdown(now + 65_000_000, now)).toBe("1:05");
    expect(formatCountdown(now + 3_723_000_000, now)).toBe("1:02:03");
    expect(formatCountdown(now + 400_000, now)).toBe("0:01");
    expect(formatCountdown(now - 5_000_000, now)).toBe("0:00");
  });
});
