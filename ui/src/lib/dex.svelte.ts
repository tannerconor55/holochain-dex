// App state: polling, signals, the maker's auto-run, and the activity feed.
//
// Signals are hints, never state (see dex_api::DexSignal). On any signal the
// store re-reads and calls idempotent externs; it also polls, so a missed
// signal only delays settlement.

import { amt, b64, shortHash, type Amounts, type BalanceView, type BookView, type DexApi, type DexSignal, type MyOrder, type ParkStatus } from "./api";
import { formatAmount } from "./format";

export const POLL_MS = 10_000;
const MAX_ACTIVITY = 50;

export type ActivityKind =
  | "created"
  | "partial"
  | "filled"
  | "cancelled"
  | "expired"
  | "incoming"
  | "settled"
  | "refunded"
  | "collected"
  | "failed";

export interface Activity {
  id: number;
  at: number;
  kind: ActivityKind;
  text: string;
}

export class DexStore {
  balance = $state<BalanceView | null>(null);
  book = $state<BookView | null>(null);
  orders = $state<MyOrder[] | null>(null);
  parks = $state<ParkStatus[] | null>(null);
  activity = $state<Activity[]>([]);
  /** The last failed read, cleared by the next successful one. */
  readError = $state<string | null>(null);
  /** Writes currently queued or running, by label. */
  pending = $state<string[]>([]);
  lastRefresh = $state<number | null>(null);

  private queue: Promise<unknown> = Promise.resolve();
  private nextId = 0;
  private stopFns: (() => void)[] = [];
  private seenStatus = new Map<string, MyOrder["status"]>();
  private seenSettled = new Set<string>();
  private baselined = false;

  constructor(readonly api: DexApi) {}

  start(): void {
    this.stopFns.push(this.api.onSignal((s) => void this.onSignal(s)));
    const timer = setInterval(() => void this.tick(), POLL_MS);
    this.stopFns.push(() => clearInterval(timer));
    void this.tick();
  }

  stop(): void {
    this.stopFns.forEach((f) => f());
    this.stopFns = [];
  }

  /** One poll: read everything, then settle what the reads say needs settling. */
  async tick(): Promise<void> {
    await this.refresh();
    await this.maintain();
  }

  /**
   * Re-read every view. Each read is independent: a failed read keeps the
   * previous value, so an eventually-consistent miss never blanks the screen.
   */
  async refresh(): Promise<void> {
    const errors: string[] = [];
    const read = async <T>(what: string, f: () => Promise<T>, set: (v: T) => void) => {
      try {
        set(await f());
      } catch (e) {
        errors.push(`${what}: ${message(e)}`);
      }
    };
    await Promise.all([
      read("balance", this.api.balance, (v) => (this.balance = v)),
      read("order book", this.api.book, (v) => (this.book = v)),
      read("my orders", this.api.myOrders, (v) => {
        this.diffOrders(v);
        this.orders = v;
      }),
      read("my parks", this.api.myParks, (v) => {
        this.diffParks(v);
        this.parks = v;
      }),
    ]);
    this.baselined = true;
    this.readError = errors.length ? errors.join("; ") : null;
    this.lastRefresh = Date.now();
  }

  /** The maker's auto-run and the receiver's auto-collect. Idempotent. */
  async maintain(): Promise<void> {
    const now = Date.now() * 1000;
    const needsRun = (this.orders ?? []).some(
      (o) =>
        o.pending.length > 0 ||
        (!o.state.closed && o.state.remaining_lots > 0 && now >= o.state.terms.expires_at),
    );
    if (needsRun) {
      const reports = await this.write("Settling my orders", this.api.runMyOrders);
      if (reports?.length) await this.refresh();
    }
    const u = this.balance?.uncollected;
    if (u && Object.keys(u).length > 0) {
      const collected = await this.write("Collecting", this.api.collectAll);
      if (collected?.length) {
        this.log("collected", `Collected ${formatPair(u)} into your balance`);
        await this.refresh();
      }
    }
  }

  private async onSignal(signal: DexSignal): Promise<void> {
    if (signal.type === "park_placed") {
      this.log("incoming", `A taker parked for ${signal.lots} lots on order ${shortHash(signal.escrow)}`);
    } else {
      this.log("settled", `Order ${shortHash(signal.escrow)} ran a ${signal.mode.toLowerCase()}: collecting`);
    }
    await this.tick();
  }

  /**
   * Run a write after every earlier one. Concurrent writes from one agent
   * race for the source chain head, so they are serialized. Failures are
   * logged, not thrown: the caller gets `undefined`.
   */
  write<T>(label: string, f: () => Promise<T>): Promise<T | undefined> {
    this.pending = [...this.pending, label];
    const run = this.queue.then(async () => {
      try {
        return await f();
      } catch (e) {
        this.log("failed", `${label} failed: ${message(e)}`);
        return undefined;
      } finally {
        const i = this.pending.indexOf(label);
        this.pending = this.pending.filter((_, j) => j !== i);
      }
    });
    this.queue = run;
    return run;
  }

  log(kind: ActivityKind, text: string): void {
    this.activity = [{ id: this.nextId++, at: Date.now(), kind, text }, ...this.activity].slice(0, MAX_ACTIVITY);
  }

  // Activity from diffing polls. The first successful read is a baseline and
  // logs nothing, so reloading the page does not replay history.

  private diffOrders(orders: MyOrder[]): void {
    for (const o of orders) {
      const key = b64(o.state.escrow);
      const before = this.seenStatus.get(key);
      this.seenStatus.set(key, o.status);
      if (!this.baselined || before === o.status) continue;
      const label = orderLabel(o);
      const text: Record<MyOrder["status"], string> = {
        Open: `Order created: ${label}`,
        Partial: `Order partially filled: ${o.state.filled_lots}/${o.state.terms.lots} lots, ${label}`,
        Filled: `Order filled: ${label}`,
        Cancelled: `Order cancelled: ${label}`,
        Expired: `Order expired: ${label}`,
      };
      const kind: Record<MyOrder["status"], ActivityKind> = {
        Open: "created",
        Partial: "partial",
        Filled: "filled",
        Cancelled: "cancelled",
        Expired: "expired",
      };
      this.log(kind[o.status], text[o.status]);
    }
  }

  private diffParks(parks: ParkStatus[]): void {
    for (const p of parks) {
      if (!p.settlement) continue;
      const key = b64(p.park);
      if (this.seenSettled.has(key)) continue;
      this.seenSettled.add(key);
      if (!this.baselined) continue;
      const filled = p.settlement.filled_lots;
      if (filled === 0) {
        this.log("refunded", `Take on order ${shortHash(p.escrow)} refunded: ${formatPair(p.amounts)} returned`);
      } else {
        this.log("settled", `Trade settled: ${filled} of ${p.requested_lots} lots on order ${shortHash(p.escrow)}`);
      }
    }
  }
}

export function orderLabel(o: MyOrder): string {
  const t = o.state.terms;
  return `${t.side === "Sell" ? "sell" : "buy"} ${t.lots} A @ ${formatAmount(t.price_per_lot)} HF`;
}

/** "12.00 A + 3.50 HF": every unit held, in unit order ("0.00" if none). */
export function formatPair(x: Amounts): string {
  const parts = Object.keys(x).sort().filter((u) => amt(x, u) > 0).map((u) => `${formatAmount(amt(x, u))} ${u}`);
  return parts.length ? parts.join(" + ") : "0.00";
}

export function message(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "object" && e && "message" in e) return String((e as { message: unknown }).message);
  return String(e);
}
