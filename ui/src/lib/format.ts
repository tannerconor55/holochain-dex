// Amounts are integer minor units end to end (2 decimals: 120 = 1.20).
// Formatting happens only at display; parsing never goes through floats.
// Each unit's decimals come from the DNA properties (`get_markets`); 2 is
// the default for callers that do not pass them.

/** Minor units per whole unit at the default 2 decimals. */
export const MINOR = 100;

const DEFAULT_DECIMALS = 2;

/** Throw unless `n` is a non-negative safe integer (u64 values from the zomes). */
export function assertMinor(n: number, what = "amount"): number {
  if (!Number.isSafeInteger(n) || n < 0) {
    throw new RangeError(`${what} is not a safe non-negative integer: ${n}`);
  }
  return n;
}

/** 120 → "1.20", 123456 → "1,234.56"; at 0 decimals 120 → "120". */
export function formatAmount(minor: number, decimals = DEFAULT_DECIMALS): string {
  assertMinor(minor);
  if (decimals === 0) return minor.toLocaleString("en-US");
  const unit = BigInt(10) ** BigInt(decimals);
  const whole = BigInt(minor) / unit;
  const frac = (BigInt(minor) % unit).toString().padStart(decimals, "0");
  return `${Number(whole).toLocaleString("en-US")}.${frac}`;
}

/** A signed difference of minor units, e.g. a crossed spread: -5 → "-0.05". */
export function formatSigned(minor: number, decimals = DEFAULT_DECIMALS): string {
  return minor < 0 ? `-${formatAmount(-minor, decimals)}` : formatAmount(minor, decimals);
}

/**
 * "1.2" → 120, "1,000" → 100000. `null` for anything that is not a
 * non-negative amount with at most `decimals` decimals.
 */
export function parseAmount(text: string, decimals = DEFAULT_DECIMALS): number | null {
  const s = text.trim().replaceAll(",", "");
  const m = (decimals === 0 ? /^(\d+)()$/ : new RegExp(`^(\\d+)(?:\\.(\\d{0,${decimals}}))?$`)).exec(s);
  if (!m) return null;
  const whole = BigInt(m[1]!);
  const frac = BigInt((m[2] ?? "").padEnd(decimals, "0") || "0");
  const minor = whole * BigInt(10) ** BigInt(decimals) + frac;
  return minor <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(minor) : null;
}

/** A positive whole number of lots, or `null`. */
export function parseLots(text: string): number | null {
  const s = text.trim();
  if (!/^\d+$/.test(s)) return null;
  const n = Number(s);
  return Number.isSafeInteger(n) && n > 0 ? n : null;
}

/**
 * An exact rational price `quoteMinor / lots` (B minor units per lot) shown
 * with `decimals` places, rounded half up, using integers only:
 * (4800 + 2420) / 60 = 120.333… → "1.2033" at 4 places.
 */
export function formatAveragePrice(quoteMinor: number, lots: number, decimals = 4, quoteDecimals = DEFAULT_DECIMALS): string {
  assertMinor(quoteMinor, "quote");
  if (!Number.isSafeInteger(lots) || lots <= 0) return "—";
  // Minor units carry `quoteDecimals` already; scale by the extra places.
  decimals = Math.max(decimals, quoteDecimals);
  const extra = BigInt(10) ** BigInt(decimals - quoteDecimals);
  const scaled = (BigInt(quoteMinor) * extra * 2n + BigInt(lots)) / (2n * BigInt(lots));
  const unit = BigInt(10) ** BigInt(decimals);
  const whole = scaled / unit;
  const frac = (scaled % unit).toString().padStart(decimals, "0");
  return `${Number(whole).toLocaleString("en-US")}.${frac}`;
}

/**
 * A price change as a signed percentage of the opening price, 2 decimals,
 * integer maths (rounded toward zero): (121 − 120) / 120 → "+0.83%".
 * `null` without an opening price.
 */
export function formatChangePercent(change: number, open: number): string | null {
  if (!Number.isSafeInteger(change) || !Number.isSafeInteger(open) || open <= 0) return null;
  const bps = (BigInt(Math.abs(change)) * 10_000n) / BigInt(open);
  const whole = bps / 100n;
  const frac = (bps % 100n).toString().padStart(2, "0");
  const sign = change > 0 ? "+" : change < 0 ? "−" : "";
  return `${sign}${whole}.${frac}%`;
}

/** Percent text with up to 2 decimals → basis points: "2" → 200, "0.5" → 50. */
export function parseBps(text: string): number | null {
  return parseAmount(text);
}

/** Timestamps from the zomes are microseconds since the epoch. */
export function microsToDate(micros: number): Date {
  return new Date(Math.floor(micros / 1000));
}

export function nowMicros(): number {
  return Date.now() * 1000;
}

export function formatTime(micros: number): string {
  return microsToDate(micros).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

/** A countdown to `micros`: "29:05", "1:02:03", or "0:00" once passed. */
export function formatCountdown(micros: number, now = nowMicros()): string {
  const secs = Math.max(0, Math.ceil((micros - now) / 1_000_000));
  const [h, m, s] = [Math.floor(secs / 3600), Math.floor((secs % 3600) / 60), secs % 60];
  const pad = (n: number) => String(n).padStart(2, "0");
  return h ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

/** "in 14 min", "in 3 h", "expired". */
export function formatExpiry(expiresAt: number, now = nowMicros()): string {
  const secs = Math.floor((expiresAt - now) / 1_000_000);
  if (secs <= 0) return "expired";
  if (secs < 60) return `in ${secs} s`;
  if (secs < 3600) return `in ${Math.floor(secs / 60)} min`;
  return `in ${Math.floor(secs / 3600)} h`;
}
