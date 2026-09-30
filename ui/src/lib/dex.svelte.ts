// App state: polling, signals, the maker's auto-run, notifications and the
// activity feed.
//
// Signals are hints, never state (see dex_api::DexSignal). On any signal the
// store re-reads and calls idempotent externs; it also polls, so a missed
// signal only delays settlement. Every notification goes through `notify`
// with a key both paths derive (lib/notify.ts), so each event is shown once.

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
import { formatAmount, formatCountdown } from "./format";
import { noticeKey, Notices, toastMs, type NoticeLevel, type Toast } from "./notify";

export const POLL_MS = 10_000;

/**
 * A maker counts as online if they answered a ping or signalled within this
 * long. An online maker's app runs its orders at least every `POLL_MS`, so
 * six polls without a sign of them is worth a warning; any shorter and a
 * slow network would warn about makers who are fine.
 */
export const PRESENCE_WINDOW_MS = 60_000;

/**
 * A park unsettled this long is checked for an offline maker. An online
 * maker settles within a poll or two (signal plus 10 s poll), so a minute
 * means something is wrong; the check runs at most once per window.
 */
export const WAITING_AFTER_MS = 60_000;
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
  | "reclaimable"
  | "waiting"
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
  /** Notifications on screen, newest last. */
  toasts = $state<Toast[]>([]);
  /** The last failed read, cleared by the next successful one. */
  readError = $state<string | null>(null);
  /** Writes currently queued or running, by label. */
  pending = $state<string[]>([]);
  lastRefresh = $state<number | null>(null);

  private queue: Promise<unknown> = Promise.resolve();
  private nextId = 0;
  private stopFns: (() => void)[] = [];
  private notices = new Notices();
  private lastWaitingCheck = 0;
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
    await this.checkWaiting();
  }

  /**
   * Parks unsettled for `WAITING_AFTER_MS` are checked, once per window, for
   * a maker who does not answer; each is reported once.
   */
  async checkWaiting(now = Date.now()): Promise<void> {
    if (now - this.lastWaitingCheck < WAITING_AFTER_MS) return;
    const waiting = (this.parks ?? []).filter(
      (p) =>
        !p.settlement &&
        !p.reclaimed &&
        now * 1000 - p.parked_at >= WAITING_AFTER_MS * 1000 &&
        !this.notices.has(noticeKey.waiting(p.park)),
    );
    if (!waiting.length) return;
    this.lastWaitingCheck = now;
    let presence: MakerPresence[];
    try {
      presence = await this.checkMakers(waiting.map((p) => p.escrow));
    } catch {
      return; // advisory: try again next window
    }
    for (const p of waiting) {
      const answer = presence.find((m) => b64(m.order) === b64(p.escrow));
      if (!answer || answer.reachable || this.seenRecently(answer.maker)) continue;
      const when =
        p.deadline > now * 1000 ? `in ${formatCountdown(p.deadline, now * 1000)}` : "now, once they are back online";
      this.notify(
        noticeKey.waiting(p.park),
        "waiting",
        "warn",
        `${this.pair(p.amounts)} parked on order ${shortHash(p.escrow)} is waiting on an offline maker. You can reclaim it ${when}.`,
      );
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

  /** Handle a signal: notify at once, then re-read (the poll would too). */
  async onSignal(signal: DexSignal): Promise<void> {
    switch (signal.type) {
      case "park_placed":
        // Anyone who signals is online, whatever the signal says.
        this.markSeen(signal.taker);
        this.notify(
          noticeKey.incoming(signal.park),
          "incoming",
          "info",
          `A taker parked for ${signal.lots} lots on your order ${shortHash(signal.escrow)}: settling`,
        );
        break;
      case "run_settled":
        this.markSeen(signal.maker);
        for (const f of signal.fills ?? []) {
          this.notifyFill(f.park, signal.escrow, f.filled_lots, f.requested_lots);
        }
        break;
      case "order_updated":
        this.notifyOrder(signal.escrow, signal.status, signal.filled_lots, signal.lots, null);
        break;
    }
    if (this.baselined) await this.tick();
  }

  /**
   * Show `text` once per `key`: in the activity log, and as a toast unless
   * `quiet`. Returns whether it was new.
   */
  notify(key: string, kind: ActivityKind, level: NoticeLevel, text: string, quiet = false): boolean {
    if (!this.notices.claim(key)) return false;
    this.log(kind, text);
    if (!quiet) {
      const toast = { id: this.nextId++, key, level, text };
      this.toasts = [...this.toasts, toast];
      const ms = toastMs(level);
      if (ms !== null) setTimeout(() => this.dismiss(toast.id), ms);
    }
    return true;
  }

  dismiss(id: number): void {
    this.toasts = this.toasts.filter((t) => t.id !== id);
  }

  private notifyFill(park: ActionHash, escrow: ActionHash, filled: number, requested: number, amounts?: Amounts): void {
    const key = noticeKey.fill(park);
    if (filled === 0) {
      const what = amounts ? `: ${this.pair(amounts)} returned` : "";
      this.notify(key, "refunded", "info", `Your take on order ${shortHash(escrow)} was refunded${what}`);
    } else {
      this.notify(key, "settled", "success", `Trade settled: ${filled} of ${requested} lots on order ${shortHash(escrow)}`);
    }
  }

  private notifyOrder(
    escrow: ActionHash,
    status: MyOrder["status"],
    filled: number,
    lots: number,
    order: MyOrder | null,
  ): void {
    const label = order ? orderLabel(order, this.marketById(order.state.market), this.fmtFn) : `order ${shortHash(escrow)}`;
    const text: Record<MyOrder["status"], string> = {
      Open: `Order created: ${label}`,
      Partial: `Order partially filled: ${filled}/${lots} lots, ${label}`,
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
    const level: NoticeLevel = status === "Filled" || status === "Partial" ? "success" : "info";
    // Creating an order is the user's own action: log it, no toast.
    this.notify(noticeKey.order(escrow, status, filled), kind[status], level, text[status], status === "Open");
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
        const result = await f();
        // A later failure of the same write is news again.
        this.notices.release(noticeKey.failed(label, ""));
        return result;
      } catch (e) {
        const why = message(e);
        const level: NoticeLevel = label === "Settling my orders" || label === "Collecting" ? "error" : "warn";
        this.notify(noticeKey.failed(label, why), "failed", level, `${label} failed: ${why}`);
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

  // The poll fallback: notifications from diffing reads, with the keys the
  // signals use. The first successful read is a baseline: its history is
  // claimed silently so a reload does not replay it, but what needs action
  // now (a reclaimable park) is still shown.

  private diffOrders(orders: MyOrder[]): void {
    for (const o of orders) {
      const key = noticeKey.order(o.state.escrow, o.status, o.state.filled_lots);
      if (!this.baselined) {
        this.notices.claim(key);
        continue;
      }
      this.notifyOrder(o.state.escrow, o.status, o.state.filled_lots, o.state.terms.lots, o);
    }
  }

  private diffParks(parks: ParkStatus[]): void {
    for (const p of parks) {
      if (p.reclaimed) {
        const key = noticeKey.reclaimed(p.park);
        if (!this.baselined) this.notices.claim(key);
        else this.notify(key, "reclaimed", "success", `Reclaimed ${this.pair(p.amounts)} from order ${shortHash(p.escrow)}`);
      } else if (p.settlement) {
        if (!this.baselined) this.notices.claim(noticeKey.fill(p.park));
        else this.notifyFill(p.park, p.escrow, p.settlement.filled_lots, p.requested_lots, p.amounts);
      } else if (p.reclaimable) {
        this.notify(
          noticeKey.reclaimable(p.park),
          "reclaimable",
          "warn",
          `${this.pair(p.amounts)} parked on order ${shortHash(p.escrow)} was never settled: reclaim it under My takes`,
        );
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
