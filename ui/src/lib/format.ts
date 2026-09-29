// Amounts are integer minor units end to end (2 decimals: 120 = 1.20).
// Formatting happens only at display; parsing never goes through floats.

/** Minor units per whole unit of either asset. */
export const MINOR = 100;

/** Throw unless `n` is a non-negative safe integer (u64 values from the zomes). */
export function assertMinor(n: number, what = "amount"): number {
  if (!Number.isSafeInteger(n) || n < 0) {
    throw new RangeError(`${what} is not a safe non-negative integer: ${n}`);
  }
  return n;
}

/** 120 → "1.20", 123456 → "1,234.56". */
export function formatAmount(minor: number): string {
  assertMinor(minor);
  const whole = Math.floor(minor / MINOR);
  const frac = String(minor % MINOR).padStart(2, "0");
  return `${whole.toLocaleString("en-US")}.${frac}`;
}

/** A signed difference of minor units, e.g. a crossed spread: -5 → "-0.05". */
export function formatSigned(minor: number): string {
  return minor < 0 ? `-${formatAmount(-minor)}` : formatAmount(minor);
}

/**
 * "1.2" → 120, "1,000" → 100000. `null` for anything that is not a
 * non-negative amount with at most 2 decimals.
 */
export function parseAmount(text: string): number | null {
  const s = text.trim().replaceAll(",", "");
  const m = /^(\d+)(?:\.(\d{0,2}))?$/.exec(s);
  if (!m) return null;
  const whole = Number(m[1]);
  const frac = Number((m[2] ?? "").padEnd(2, "0"));
  const minor = whole * MINOR + frac;
  return Number.isSafeInteger(minor) ? minor : null;
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
export function formatAveragePrice(quoteMinor: number, lots: number, decimals = 4): string {
  assertMinor(quoteMinor, "quote");
  if (!Number.isSafeInteger(lots) || lots <= 0) return "—";
  // Minor units carry 2 decimals already; scale by the extra places.
  const extra = BigInt(10) ** BigInt(Math.max(0, decimals - 2));
  const scaled = (BigInt(quoteMinor) * extra * 2n + BigInt(lots)) / (2n * BigInt(lots));
  const unit = BigInt(10) ** BigInt(decimals);
  const whole = scaled / unit;
  const frac = (scaled % unit).toString().padStart(decimals, "0");
  return `${Number(whole).toLocaleString("en-US")}.${frac}`;
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

/** "in 14 min", "in 3 h", "expired". */
export function formatExpiry(expiresAt: number, now = nowMicros()): string {
  const secs = Math.floor((expiresAt - now) / 1_000_000);
  if (secs <= 0) return "expired";
  if (secs < 60) return `in ${secs} s`;
  if (secs < 3600) return `in ${Math.floor(secs / 60)} min`;
  return `in ${Math.floor(secs / 3600)} h`;
}
