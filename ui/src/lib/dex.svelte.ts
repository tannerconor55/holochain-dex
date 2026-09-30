// App state: polling, signals, the maker's auto-run, and the activity feed.
//
// Signals are hints, never state (see dex_api::DexSignal). On any signal the
// store re-reads and calls idempotent externs; it also polls, so a missed
// signal only delays settlement.

import {
  amt,
  b64,
  HUB,
  shortHash,
  UNIT_A,
  type Amounts,
  type BalanceView,
  type BookView,
  type DexApi,
  type DexConfig,
  type DexSignal,
  type MakerPresence,
  type MarketId,
  type MarketInfo,
  type MyOrder,
  type ParkStatus,
  type UnitId,
} from "./api";
import type { ActionHash, AgentPubKey } from "@holochain/client";
import { formatAmount } from "./format";

export const POLL_MS = 10_000;

/**
 * A maker counts as online if they answered a ping or signalled within this
 * long. An online maker's app runs its orders at least every `POLL_MS`, so
 * six polls without a sign of them is worth a warning; any shorter and a
 * slow network would warn about makers who are fine.
 */
export const PRESENCE_WINDOW_MS = 60_000;
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
  | "reclaimed"
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
  config = $state<DexConfig | null>(null);
  /** The selected market; `null` until the config is read, then the first. */
  marketId = $state<MarketId | null>(null);
  /** When each agent (base64) was last known online, in ms. */
  lastSeen = $state<Record<string, number>>({});
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
  private seenReclaimed = new Set<string>();
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
    if (!this.config) {
      await read("markets", this.api.config, (v) => {
        this.config = v;
        this.marketId ??= v.markets[0]?.id ?? null;
      });
    }
    const market = this.marketId;
    await Promise.all([
      read("balance", this.api.balance, (v) => (this.balance = v)),
      // A book read for a market no longer selected is dropped.
      read("order book", () => this.api.book(market), (v) => market === this.marketId && (this.book = v)),
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
        this.log("collected", `Collected ${formatPair(u, this.fmtFn)} into your balance`);
        await this.refresh();
      }
    }
  }

  // Markets and units

  get market(): MarketInfo | null {
    return this.config?.markets.find((m) => m.id === this.marketId) ?? this.config?.markets[0] ?? null;
  }

  get base(): UnitId {
    return this.market?.def.base ?? UNIT_A;
  }

  get quote(): UnitId {
    return this.market?.def.quote ?? HUB;
  }

  /** What a quantity counts: the base unit when a lot is one whole unit, else lots. */
  get quantityUnit(): string {
    const m = this.market;
    if (!m || m.def.lot_size === 10 ** m.base_decimals) return this.base;
    return `lots of ${this.fmt(m.def.lot_size, m.def.base)} ${m.def.base}`;
  }

  /** A unit's declared decimals; 2 until the config is read. */
  decimals(unit: UnitId): number {
    for (const m of this.config?.markets ?? []) {
      if (m.def.base === unit) return m.base_decimals;
      if (m.def.quote === unit) return m.quote_decimals;
    }
    return 2;
  }

  /** Minor units of `unit`, at its declared decimals. */
  fmt(minor: number, unit: UnitId): string {
    return formatAmount(minor, this.decimals(unit));
  }

  /** `fmt` as a plain function, for helpers outside the store. */
  readonly fmtFn: Fmt = (minor, unit) => this.fmt(minor, unit);

  pair(x: Amounts): string {
    return formatPair(x, this.fmtFn);
  }

  marketById(id: MarketId): MarketInfo | null {
    return this.config?.markets.find((m) => m.id === id) ?? null;
  }

  selectMarket(id: MarketId): void {
    if (id === this.marketId) return;
    this.marketId = id;
    this.book = null;
    void this.refresh();
  }

  // Maker presence (advisory: see dex_api::MakerPresence)

  markSeen(agent: AgentPubKey): void {
    this.lastSeen = { ...this.lastSeen, [b64(agent)]: Date.now() };
  }

  seenRecently(agent: AgentPubKey, now = Date.now()): boolean {
    const at = this.lastSeen[b64(agent)];
    return at !== undefined && now - at <= PRESENCE_WINDOW_MS;
  }

  /** Ping the makers of `orders`; whoever answers counts as seen. */
  async checkMakers(orders: ActionHash[]): Promise<MakerPresence[]> {
    const presence = await this.api.checkMakers(orders);
    for (const p of presence) if (p.reachable) this.markSeen(p.maker);
    return presence;
  }

  /** Take back a park its maker never settled. */
  async reclaim(park: ActionHash): Promise<void> {
    const done = await this.write("Reclaiming", () => this.api.reclaimPark(park));
    if (done) await this.refresh();
  }

  private async onSignal(signal: DexSignal): Promise<void> {
    // Anyone who signals is online, whatever the signal says.
    this.markSeen(signal.type === "park_placed" ? signal.taker : signal.maker);
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
      const label = orderLabel(o, this.marketById(o.state.market), this.fmtFn);
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
      if (p.reclaimed) {
        const key = b64(p.park);
        if (this.seenReclaimed.has(key)) continue;
        this.seenReclaimed.add(key);
        if (this.baselined) this.log("reclaimed", `Reclaimed ${formatPair(p.amounts, this.fmtFn)} from order ${shortHash(p.escrow)}`);
        continue;
      }
      if (!p.settlement) continue;
      const key = b64(p.park);
      if (this.seenSettled.has(key)) continue;
      this.seenSettled.add(key);
      if (!this.baselined) continue;
      const filled = p.settlement.filled_lots;
      if (filled === 0) {
        this.log("refunded", `Take on order ${shortHash(p.escrow)} refunded: ${formatPair(p.amounts, this.fmtFn)} returned`);
      } else {
        this.log("settled", `Trade settled: ${filled} of ${p.requested_lots} lots on order ${shortHash(p.escrow)}`);
      }
    }
  }
}

/** Formats minor units of a unit; the store's `fmt` reads declared decimals. */
export type Fmt = (minor: number, unit: UnitId) => string;
const twoDecimals: Fmt = (minor) => formatAmount(minor);

/** "sell 100 lots A @ 1.20 HF", in the order's own market. */
export function orderLabel(o: MyOrder, market: MarketInfo | null, fmt: Fmt = twoDecimals): string {
  const t = o.state.terms;
  const [base, quote] = market ? [market.def.base, market.def.quote] : [UNIT_A, HUB];
  return `${t.side === "Sell" ? "sell" : "buy"} ${t.lots} ${base} @ ${fmt(t.price_per_lot, quote)} ${quote}`;
}

/** "12.00 A + 3.50 HF": every unit held, in unit order ("0.00" if none). */
export function formatPair(x: Amounts, fmt: Fmt = twoDecimals): string {
  const parts = Object.keys(x).sort().filter((u) => amt(x, u) > 0).map((u) => `${fmt(amt(x, u), u)} ${u}`);
  return parts.length ? parts.join(" + ") : "0.00";
}

export function message(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "object" && e && "message" in e) return String((e as { message: unknown }).message);
  return String(e);
}
