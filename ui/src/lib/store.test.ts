// DexStore's notifications against a fake API: a signal and the next poll
// notify once; a missed signal is caught by the poll; the first poll does
// not replay history; failures notify once until the write works again.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DexApi, DexSignal, MyOrder, ParkStatus, MakerPresence, BalanceView } from "./api";
import { DexStore } from "./dex.svelte";

const hash = (byte: number) => new Uint8Array(39).fill(byte);
const PARK = hash(1);
const ESCROW = hash(2);
const MAKER = hash(3);

const empty = { available: {}, locked_in_escrows: {}, parked: {}, uncollected: {}, total: {} };

function park(over: Partial<ParkStatus> = {}): ParkStatus {
  const now = Date.now() * 1000;
  return {
    park: PARK,
    escrow: ESCROW,
    amounts: { HF: 4_800 },
    requested_lots: 40,
    parked_at: now - 5_000_000,
    settlement: null,
    deadline: now + 1_800_000_000,
    reclaimed: null,
    reclaimable: false,
    ...over,
  };
}

function order(status: MyOrder["status"], filled: number): MyOrder {
  return {
    state: {
      escrow: ESCROW,
      maker: MAKER,
      market: "m",
      terms: { side: "Sell", price_per_lot: 120, lots: 40, expires_at: Number.MAX_SAFE_INTEGER },
      opened_at: 0,
      locked: {},
      remaining_lots: 40 - filled,
      filled_lots: filled,
      runs: 0,
      expired: false,
      closed: status === "Cancelled" || status === "Filled",
      released_at: null,
    },
    pending: [],
    status,
  };
}

/** A fake API whose state the test changes between polls. */
function fake() {
  const state = {
    parks: [] as ParkStatus[],
    orders: [] as MyOrder[],
    balance: empty as BalanceView,
    presence: [] as MakerPresence[],
    runMyOrders: async () => [] as unknown[],
  };
  const api = {
    me: hash(9),
    config: async () => ({ park_timeout_secs: 1800, settle_grace_secs: 300, markets: [] }),
    balance: async () => state.balance,
    book: async () => ({ asks: [], bids: [], spread: null }),
    myOrders: async () => state.orders,
    myParks: async () => state.parks,
    runMyOrders: () => state.runMyOrders(),
    collectAll: async () => [],
    checkMakers: async () => state.presence,
    recentTrades: async () => [],
    marketStats: async () => null,
    candles: async () => [],
    onSignal: () => () => undefined,
  };
  return { state, store: new DexStore(api as unknown as DexApi) };
}

const texts = (store: DexStore) => store.toasts.map((t) => t.text);
const logged = (store: DexStore, kind: string) => store.activity.filter((a) => a.kind === kind).length;

const settled = (filled: number): DexSignal => ({
  type: "run_settled",
  escrow: ESCROW,
  run: hash(4),
  maker: MAKER,
  mode: "Fill",
  fills: [{ park: PARK, filled_lots: filled, requested_lots: 40 }],
});

beforeEach(() => vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] }));
afterEach(() => vi.useRealTimers());

describe("DexStore notifications", () => {
  it("shows a trade once when the signal and then the poll report it", async () => {
    const { state, store } = fake();
    state.parks = [park()];
    await store.tick(); // baseline: the park is waiting
    await store.onSignal(settled(40)); // the run's signal, before this node sees the run
    expect(texts(store)).toEqual(["Trade settled: 40 of 40 lots on order " + store.toasts[0]!.text.slice(-6)]);
    state.parks = [park({ settlement: { run: hash(4), filled_lots: 40 } })];
    await store.tick(); // the poll now sees it
    await store.tick();
    expect(store.toasts).toHaveLength(1);
    expect(logged(store, "settled")).toBe(1);
  });

  it("catches a missed signal on the next poll", async () => {
    const { state, store } = fake();
    state.parks = [park()];
    await store.tick();
    state.parks = [park({ settlement: { run: hash(4), filled_lots: 0 } })]; // refunded; no signal came
    await store.tick();
    expect(texts(store)).toHaveLength(1);
    expect(texts(store)[0]).toMatch(/refunded: 48\.00 HF returned/);
  });

  it("shows each order status and each partial fill once, from either path", async () => {
    const { state, store } = fake();
    state.orders = [order("Open", 0)];
    await store.tick();
    await store.onSignal({ type: "order_updated", escrow: ESCROW, run: hash(5), status: "Partial", filled_lots: 10, lots: 40 });
    state.orders = [order("Partial", 10)];
    await store.tick();
    expect(texts(store)).toEqual([expect.stringMatching(/partially filled: 10\/40/)]);
    state.orders = [order("Partial", 25)];
    await store.tick();
    state.orders = [order("Filled", 40)];
    await store.tick();
    expect(texts(store)).toEqual([
      expect.stringMatching(/10\/40/),
      expect.stringMatching(/25\/40/),
      expect.stringMatching(/^Order filled/),
    ]);
  });

  it("does not replay history on the first poll, but shows what needs action", async () => {
    const { state, store } = fake();
    state.orders = [order("Cancelled", 20)];
    state.parks = [
      park({ settlement: { run: hash(4), filled_lots: 40 } }),
      park({ park: hash(7), reclaimable: true, deadline: Date.now() * 1000 - 1 }),
    ];
    await store.tick();
    expect(texts(store)).toEqual([expect.stringMatching(/never settled: reclaim it/)]);
    expect(store.toasts[0]!.level).toBe("warn");
  });

  it("reports a failing settlement once, and again only after it has worked", async () => {
    const { state, store } = fake();
    state.orders = [{ ...order("Partial", 10), pending: [{ park: PARK, taker: hash(8), amounts: {}, requested_lots: 1, parked_at: 0 }] }];
    state.runMyOrders = async () => {
      throw new Error("source chain head moved");
    };
    await store.tick();
    await store.tick();
    expect(texts(store)).toEqual(["Settling my orders failed: source chain head moved"]);
    expect(store.toasts[0]!.level).toBe("error");
    state.runMyOrders = async () => [];
    await store.tick();
    state.runMyOrders = async () => {
      throw new Error("source chain head moved");
    };
    await store.tick();
    expect(texts(store).filter((t) => t.startsWith("Settling my orders failed"))).toHaveLength(2);
  });

  it("warns once about funds waiting on a maker who does not answer", async () => {
    const { state, store } = fake();
    state.parks = [park({ parked_at: (Date.now() - 120_000) * 1000 })];
    state.presence = [{ order: ESCROW, maker: MAKER, reachable: false, detail: "not reachable" }];
    await store.tick();
    expect(texts(store)).toEqual([expect.stringMatching(/waiting on an offline maker\. You can reclaim it in /)]);
    await store.checkWaiting(Date.now() + 120_000); // the next window
    expect(store.toasts).toHaveLength(1);
  });

  it("does not warn about a maker who answers", async () => {
    const { state, store } = fake();
    state.parks = [park({ parked_at: (Date.now() - 120_000) * 1000 })];
    state.presence = [{ order: ESCROW, maker: MAKER, reachable: true, detail: null }];
    await store.tick();
    expect(store.toasts).toHaveLength(0);
  });

  it("fades news toasts and keeps errors until dismissed", async () => {
    const { store } = fake();
    store.notify("a", "settled", "success", "news");
    store.notify("b", "failed", "error", "problem");
    vi.advanceTimersByTime(8_001);
    expect(texts(store)).toEqual(["problem"]);
    store.dismiss(store.toasts[0]!.id);
    expect(store.toasts).toHaveLength(0);
    expect(store.activity.map((a) => a.text)).toEqual(["problem", "news"]);
  });
});
