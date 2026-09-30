// The only place the UI talks to the conductor.
//
// Types are hand-mirrored from the Rust crates `ledger_api`, `dex_api` and
// `dex_core` (field names are the msgpack wire format). Keep them in this one
// file so drift is visible. On a serialization error, first rebuild the wasm
// and repack (`nix develop -c ./build.sh`), then compare these types with the
// Rust structs. Never bump msgpack/serde versions as a first response.

import {
  AdminWebsocket,
  AppWebsocket,
  CellType,
  SignalType,
  encodeHashToBase64,
  type ActionHash,
  type AgentPubKey,
  type RoleNameCallZomeRequest,
  type Signal,
} from "@holochain/client";

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/** A maker's side. For a taker, `Buy` takes asks and `Sell` takes bids. */
export type Side = "Sell" | "Buy";
export type RunMode = "Fill" | "Release";
export type OrderStatus = "Open" | "Partial" | "Filled" | "Cancelled" | "Expired";

/** A unit's id, as the DNA properties declare it. */
export type UnitId = string;
/** The default market's base unit. */
export const UNIT_A: UnitId = "A";
/** HF, the hub unit every market is quoted in. */
export const HUB: UnitId = "HF";

/**
 * Minor units by unit id (dex_core::Amounts). Normalised on the Rust side:
 * a zero-valued unit is absent, and a map holding a zero is refused.
 */
export type Amounts = Record<UnitId, number>;

/** The amount of `unit`; absent means zero. */
export const amt = (x: Amounts | null | undefined, unit: UnitId): number => x?.[unit] ?? 0;

/** Build an Amounts with zero units left out, as the zomes require. */
export function amounts(entries: [UnitId, number][]): Amounts {
  return Object.fromEntries(entries.filter(([, n]) => n > 0));
}

/** dex_core::MarketId: 64 lowercase hex characters. */
export type MarketId = string;

/** dex_core::MarketDef */
export interface MarketDef {
  base: UnitId;
  quote: UnitId;
  /** Base minor units per lot. */
  lot_size: number;
  /** Prices are multiples of this, in quote minor units per lot. */
  tick_size: number;
}

export interface MarketInfo {
  id: MarketId;
  def: MarketDef;
  base_decimals: number;
  quote_decimals: number;
}

/** dex_api::DexConfig */
export interface DexConfig {
  park_timeout_secs: number;
  settle_grace_secs: number;
  /** The first is the default market. */
  markets: MarketInfo[];
}

export interface OrderTerms {
  side: Side;
  /** Quote (HF) minor units per lot. */
  price_per_lot: number;
  lots: number;
  /** Microseconds since the epoch. */
  expires_at: number;
}

export interface EscrowState {
  escrow: ActionHash;
  maker: AgentPubKey;
  market: MarketId;
  terms: OrderTerms;
  opened_at: number;
  locked: Amounts;
  remaining_lots: number;
  filled_lots: number;
  runs: number;
  expired: boolean;
  closed: boolean;
  released_at: number | null;
}

export interface PendingPark {
  park: ActionHash;
  taker: AgentPubKey;
  amounts: Amounts;
  requested_lots: number;
  parked_at: number;
}

/** §23: total = available + locked + parked + uncollected. */
export interface BalanceView {
  available: Amounts;
  locked_in_escrows: Amounts;
  parked: Amounts;
  uncollected: Amounts;
  total: Amounts;
}

export interface RunReport {
  run: ActionHash;
  mode: RunMode;
  filled_lots: number;
  fills: { park: ActionHash; filled_lots: number }[];
  locked: Amounts;
  still_pending: number;
  receivers: AgentPubKey[];
}

export interface ParkStatus {
  park: ActionHash;
  escrow: ActionHash;
  amounts: Amounts;
  requested_lots: number;
  parked_at: number;
  settlement: { run: ActionHash; filled_lots: number } | null;
  /** No run may consume the park after this (µs). */
  deadline: number;
  reclaimed: ActionHash | null;
  /** `reclaimPark` should succeed now. */
  reclaimable: boolean;
}

export interface PriceLevel {
  price_per_lot: number;
  lots: number;
  orders: number;
}

export interface BookView {
  asks: PriceLevel[];
  bids: PriceLevel[];
  /** Best ask − best bid; negative when crossed. */
  spread: number | null;
}

export interface Order {
  id: ActionHash;
  maker: AgentPubKey;
  side: Side;
  price_per_lot: number;
  remaining_lots: number;
  opened_at: number;
  expires_at: number;
  closed: boolean;
}

export interface PlannedFill {
  order: ActionHash;
  price_per_lot: number;
  lots: number;
  cost: Amounts;
  receives: Amounts;
}

export interface TakePlan {
  fills: PlannedFill[];
  filled: number;
  shortfall: number;
  total_cost: Amounts;
  total_receives: Amounts;
}

export interface TakeRequest {
  market: MarketId | null;
  take: Side;
  lots: number;
  limit_price: number | null;
}

export interface TakeResult {
  plan: TakePlan;
  parks: { escrow: ActionHash; park: ActionHash; lots: number }[];
}

export interface MyOrder {
  state: EscrowState;
  pending: PendingPark[];
  status: OrderStatus;
}

/** Whole lots, or a budget of the paying asset (B to buy, A to sell). */
export type MarketAmount = { Lots: number } | { Budget: number };

/** dex_core::book::market::DEFAULT_MAX_SLIPPAGE_BPS */
export const DEFAULT_MAX_SLIPPAGE_BPS = 200;

export interface MarketPlan {
  take: Side;
  plan: TakePlan;
  reference_price: number | null;
  max_slippage_bps: number | null;
  limit_price: number;
  /** Average price = total_quote_minor / total_lots (B minor units per lot). */
  total_quote_minor: number;
  total_lots: number;
  worst_price: number | null;
  unspent_budget: number | null;
}

export interface FillChange {
  order: ActionHash;
  expected_lots: number;
  planned_lots: number;
}

export interface MarketResult {
  market: MarketId;
  plan: MarketPlan;
  parks: { escrow: ActionHash; park: ActionHash; lots: number }[];
  changes: FillChange[];
  /** 0 for the order, 1 for its one retry. */
  attempt: number;
}

export interface MarketRetry {
  unfilled_lots: number;
  still_pending: number;
  result: MarketResult | null;
}

/** dex_api::MakerPresence. Advisory: proves nothing about later. */
export interface MakerPresence {
  order: ActionHash;
  maker: AgentPubKey;
  reachable: boolean;
  detail: string | null;
}

export type DexSignal =
  | { type: "park_placed"; escrow: ActionHash; park: ActionHash; taker: AgentPubKey; lots: number }
  | { type: "run_settled"; escrow: ActionHash; run: ActionHash; maker: AgentPubKey; mode: RunMode };

// ---------------------------------------------------------------------------
// Calls
// ---------------------------------------------------------------------------

export const ROLE = "dex";

/** The subset of `AppWebsocket` the API needs; stubbed in tests. */
export interface ZomeClient {
  readonly myPubKey: AgentPubKey;
  callZome<T>(request: RoleNameCallZomeRequest): Promise<T>;
  on(event: "signal", listener: (signal: Signal) => void): () => void;
}

export class DexApi {
  constructor(private readonly client: ZomeClient) {}

  get me(): AgentPubKey {
    return this.client.myPubKey;
  }

  private call<T>(zome_name: "ledger" | "dex", fn_name: string, payload: unknown = null): Promise<T> {
    return this.client.callZome<T>({ role_name: ROLE, zome_name, fn_name, payload });
  }

  // ledger: wallet
  mint = (amounts: Amounts) => this.call<ActionHash>("ledger", "mint", amounts);
  balance = () => this.call<BalanceView>("ledger", "get_balance");
  collectAll = () => this.call<ActionHash[]>("ledger", "collect_all");
  reclaimPark = (park: ActionHash) => this.call<ActionHash>("ledger", "reclaim_park", park);

  // dex: markets and the book. `market: null` is the first declared market.
  config = () => this.call<DexConfig>("dex", "get_config");
  book = (market: MarketId | null) => this.call<BookView>("dex", "get_order_book", market);
  levelOrders = (market: MarketId | null, side: Side, price_per_lot: number) =>
    this.call<Order[]>("dex", "get_level_orders", { market, side, price_per_lot });
  planTake = (request: TakeRequest) => this.call<TakePlan>("dex", "plan_take", request);
  /** Ping the makers of `orders` (all at once; up to ~60 s if one is offline). */
  checkMakers = (orders: ActionHash[]) => this.call<MakerPresence[]>("dex", "check_makers", orders);

  // dex: writes
  placeOrder = (market: MarketId | null, terms: OrderTerms) =>
    this.call<ActionHash>("dex", "place_order", { market, terms });
  take = (request: TakeRequest) => this.call<TakeResult>("dex", "take", request);
  cancelOrder = (escrow: ActionHash) => this.call<RunReport[]>("dex", "cancel_order", escrow);
  runMyOrders = () => this.call<RunReport[]>("dex", "run_my_orders");

  // dex: market orders (taker-only, immediate-or-cancel)
  previewMarket = (market: MarketId | null, side: Side, amount: MarketAmount, max_slippage_bps: number | null) =>
    this.call<MarketPlan>("dex", "preview_market_order", { market, side, amount, max_slippage_bps });
  marketOrder = (
    market: MarketId | null,
    side: Side,
    amount: MarketAmount,
    max_slippage_bps: number | null,
    expected: MarketPlan | null,
  ) =>
    "Lots" in amount
      ? this.call<MarketResult>("dex", "market_order", { market, side, lots: amount.Lots, max_slippage_bps, expected })
      : this.call<MarketResult>("dex", "market_order_by_budget", {
          market,
          side,
          budget: amount.Budget,
          max_slippage_bps,
          expected,
        });
  /** Once per market order: callers must not loop it. */
  retryMarketShortfall = (original: MarketResult) =>
    this.call<MarketRetry>("dex", "retry_market_shortfall", { original });

  // dex: the caller's own view
  myOrders = () => this.call<MyOrder[]>("dex", "my_orders");
  myParks = () => this.call<ParkStatus[]>("dex", "my_parks");

  /** Subscribe to dex signals from this cell; everything else is ignored. */
  onSignal(listener: (signal: DexSignal) => void): () => void {
    return this.client.on("signal", (signal) => {
      const parsed = parseDexSignal(signal);
      if (parsed) listener(parsed);
    });
  }
}

export function parseDexSignal(signal: Signal): DexSignal | null {
  if (signal.type !== SignalType.App || signal.value.zome_name !== "dex") return null;
  const payload = signal.value.payload as { type?: unknown } | null;
  if (payload?.type === "park_placed" || payload?.type === "run_settled") {
    return payload as DexSignal;
  }
  return null;
}

// ---------------------------------------------------------------------------
// Identity helpers
// ---------------------------------------------------------------------------

export const b64 = (hash: Uint8Array): string => encodeHashToBase64(hash);

/** A short, stable label for a hash: the last 6 base64 characters. */
export const shortHash = (hash: Uint8Array): string => b64(hash).slice(-6);

export const sameHash = (x: Uint8Array, y: Uint8Array): boolean =>
  x.length === y.length && x.every((byte, i) => byte === y[i]);

// ---------------------------------------------------------------------------
// Connecting
// ---------------------------------------------------------------------------

/**
 * Connect to the conductor.
 *
 * - Under `hc-spin` (`npm start`): the launcher injects the app port and token;
 *   `AppWebsocket.connect()` picks them up.
 * - Against a plain sandbox: open the page with `?admin_port=…&app_port=…`
 *   (and optionally `&app_id=…`, default `dex`). A token is issued and
 *   signing credentials authorized through the admin interface.
 */
export async function connect(search = window.location.search): Promise<DexApi> {
  const params = new URLSearchParams(search);
  const adminPort = params.get("admin_port");
  const appPort = params.get("app_port");
  if (!adminPort || !appPort) {
    return new DexApi(await AppWebsocket.connect());
  }

  const appId = params.get("app_id") ?? "dex";
  const admin = await AdminWebsocket.connect({ url: new URL(`ws://localhost:${adminPort}`) });
  const { token } = await admin.issueAppAuthenticationToken({ installed_app_id: appId });
  const client = await AppWebsocket.connect({ url: new URL(`ws://localhost:${appPort}`), token });
  const info = await client.appInfo();
  const cell = info?.cell_info[ROLE]?.find((c) => c.type === CellType.Provisioned);
  if (!cell || cell.type !== CellType.Provisioned) {
    throw new Error(`app ${appId} has no provisioned ${ROLE} cell`);
  }
  await admin.authorizeSigningCredentials(cell.value.cell_id);
  return new DexApi(client);
}
