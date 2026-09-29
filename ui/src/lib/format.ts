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
