import { SignalType, type RoleNameCallZomeRequest, type Signal } from "@holochain/client";
import { describe, expect, it } from "vitest";
import { DexApi, parseDexSignal, sameHash, type ZomeClient } from "./api";

const hash = (byte: number) => new Uint8Array(39).fill(byte);

function stubClient(response: unknown = null) {
  const calls: RoleNameCallZomeRequest[] = [];
  const listeners: ((s: Signal) => void)[] = [];
  const client: ZomeClient = {
    myPubKey: hash(9),
    async callZome<T>(request: RoleNameCallZomeRequest) {
      calls.push(request);
      return response as T;
    },
    on(_event, listener) {
      listeners.push(listener);
      return () => undefined;
    },
  };
  return { client, calls, emit: (s: Signal) => listeners.forEach((l) => l(s)) };
}

const appSignal = (zome_name: string, payload: unknown): Signal => ({
  type: SignalType.App,
  value: { cell_id: [hash(1), hash(2)], zome_name, payload },
});

describe("DexApi", () => {
  it("routes wallet calls to the ledger zome and book calls to the dex zome", async () => {
    const { client, calls } = stubClient();
    const api = new DexApi(client);
    await api.mint({ a: 10_000, b: 0 });
    await api.book();
    await api.levelOrders("Sell", 120);
    await api.runMyOrders();
    expect(calls).toEqual([
      { role_name: "dex", zome_name: "ledger", fn_name: "mint", payload: { a: 10_000, b: 0 } },
      { role_name: "dex", zome_name: "dex", fn_name: "get_order_book", payload: null },
      { role_name: "dex", zome_name: "dex", fn_name: "get_level_orders", payload: { side: "Sell", price_per_lot: 120 } },
      { role_name: "dex", zome_name: "dex", fn_name: "run_my_orders", payload: null },
    ]);
  });

  it("passes order terms and take requests through unchanged", async () => {
    const { client, calls } = stubClient();
    const api = new DexApi(client);
    const terms = { side: "Sell" as const, price_per_lot: 120, lots: 100, expires_at: 1 };
    await api.placeOrder(terms);
    await api.take({ take: "Buy", lots: 40, limit_price: null });
    expect(calls.map((c) => [c.fn_name, c.payload])).toEqual([
      ["place_order", terms],
      ["take", { take: "Buy", lots: 40, limit_price: null }],
    ]);
  });

  it("routes market orders by amount kind and passes the slippage and preview", async () => {
    const { client, calls } = stubClient();
    const api = new DexApi(client);
    await api.previewMarket("Buy", { Lots: 10 }, 200);
    await api.marketOrder("Buy", { Lots: 10 }, 200, null);
    await api.marketOrder("Sell", { Budget: 5_000 }, null, null);
    expect(calls.map((c) => [c.fn_name, c.payload])).toEqual([
      ["preview_market_order", { side: "Buy", amount: { Lots: 10 }, max_slippage_bps: 200 }],
      ["market_order", { side: "Buy", lots: 10, max_slippage_bps: 200, expected: null }],
      ["market_order_by_budget", { side: "Sell", budget: 5_000, max_slippage_bps: null, expected: null }],
    ]);
  });

  it("delivers only dex signals to listeners", () => {
    const { client, emit } = stubClient();
    const seen: string[] = [];
    new DexApi(client).onSignal((s) => seen.push(s.type));
    emit(appSignal("dex", { type: "park_placed", escrow: hash(1), park: hash(2), taker: hash(3), lots: 4 }));
    emit(appSignal("ledger", { type: "park_placed" }));
    emit(appSignal("dex", { type: "something_else" }));
    emit(appSignal("dex", { type: "run_settled", escrow: hash(1), run: hash(5), maker: hash(6), mode: "Fill" }));
    expect(seen).toEqual(["park_placed", "run_settled"]);
  });
});

describe("parseDexSignal", () => {
  it("ignores system signals and malformed payloads", () => {
    expect(parseDexSignal({ type: SignalType.System, value: { SuccessfulCountersigning: hash(1) } } as Signal)).toBeNull();
    expect(parseDexSignal(appSignal("dex", null))).toBeNull();
    expect(parseDexSignal(appSignal("dex", "park_placed"))).toBeNull();
  });
});

describe("sameHash", () => {
  it("compares bytes", () => {
    expect(sameHash(hash(1), hash(1))).toBe(true);
    expect(sameHash(hash(1), hash(2))).toBe(false);
    expect(sameHash(hash(1), new Uint8Array(3).fill(1))).toBe(false);
  });
});
