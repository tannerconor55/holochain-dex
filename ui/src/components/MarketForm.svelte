<script lang="ts">
  import { b64, sameHash, shortHash, type MarketAmount, type MarketPlan, type MarketResult, type Side } from "../lib/api";
  import { message, type DexStore } from "../lib/dex.svelte";
  import { formatAmount, formatAveragePrice, parseAmount, parseBps, parseLots } from "../lib/format";

  let { store }: { store: DexStore } = $props();

  let side = $state<Side>("Buy");
  let by = $state<"lots" | "budget">("lots");
  let quantity = $state("10");
  let budget = $state("10");
  let slippage = $state("2");

  let plan = $state<MarketPlan | null>(null);
  let planError = $state<string | null>(null);
  /** The order and, once used, its one retry. */
  let results = $state<MarketResult[]>([]);
  let retryNote = $state<string | null>(null);

  let buying = $derived(side === "Buy");
  let payAsset = $derived(buying ? "B" : "A");
  let getAsset = $derived(buying ? "A" : "B");
  let pay = (x: { a: number; b: number }) => (buying ? x.b : x.a);
  let get = (x: { a: number; b: number }) => (buying ? x.a : x.b);

  let bps = $derived(parseBps(slippage));
  let amount = $derived.by((): MarketAmount | null => {
    if (by === "lots") {
      const lots = parseLots(quantity);
      return lots === null ? null : { Lots: lots };
    }
    const b = parseAmount(budget);
    return b === null || b === 0 ? null : { Budget: b };
  });
  let inputError = $derived.by(() => {
    if (bps === null) return "Slippage is a percentage with at most two decimals, e.g. 2 or 0.5.";
    if (!buying && bps > 10_000) return "A sell cannot allow more than 100% slippage.";
    if (!amount) return by === "lots" ? "Enter a whole number of A." : `Enter a budget in ${payAsset}.`;
    return null;
  });

  // Debounced preview; a late response for an old input is dropped.
  let seq = 0;
  $effect(() => {
    const request = !inputError && amount && bps !== null ? { side, amount, bps } : null;
    const mine = ++seq;
    plan = null;
    planError = null;
    if (!request) return;
    const timer = setTimeout(() => {
      store.api
        .previewMarket(request.side, request.amount, request.bps)
        .then((p) => mine === seq && (plan = p))
        .catch((e) => mine === seq && (planError = message(e)));
    }, 300);
    return () => clearTimeout(timer);
  });

  let available = $derived(store.balance ? pay(store.balance.available) : null);
  let blocked = $derived.by(() => {
    if (inputError) return inputError;
    if (planError) return planError;
    if (!plan) return "Previewing…";
    if (plan.plan.filled === 0) return "Nothing within your slippage limit.";
    if (available !== null && pay(plan.plan.total_cost) > available) {
      return `This costs ${formatAmount(pay(plan.plan.total_cost))} ${payAsset}; you have ${formatAmount(available)} ${payAsset} available.`;
    }
    return null;
  });

  async function confirm() {
    if (blocked || !plan || !amount || bps === null) return;
    const expected = $state.snapshot(plan) as MarketPlan;
    const request = { side, amount, bps };
    const done = await store.write("Market order", () =>
      store.api.marketOrder(request.side, request.amount, request.bps, expected),
    );
    if (done) {
      results = [done];
      retryNote = null;
      await store.refresh();
    }
  }

  // Result tracking, from the ledger's view of each park.
  function settlement(park: Uint8Array) {
    return store.parks?.find((p) => sameHash(p.park, park))?.settlement ?? null;
  }
  let allParks = $derived(results.flatMap((r) => r.parks));
  let waiting = $derived(allParks.filter((p) => !settlement(p.park)).length);
  let filledLots = $derived(allParks.reduce((n, p) => n + (settlement(p.park)?.filled_lots ?? 0), 0));
  let refundedLots = $derived(
    allParks.reduce((n, p) => {
      const s = settlement(p.park);
      return n + (s ? p.lots - s.filled_lots : 0);
    }, 0),
  );
  let original = $derived(results[0] ?? null);
  let unfilled = $derived(original ? original.plan.plan.shortfall + (results.length === 1 ? refundedLots : 0) : 0);
  let canRetry = $derived(results.length === 1 && waiting === 0 && unfilled > 0);

  async function retry() {
    if (!original || !canRetry) return;
    const snapshot = $state.snapshot(original) as MarketResult;
    const r = await store.write("Retrying remainder", () => store.api.retryMarketShortfall(snapshot));
    if (!r) return;
    const limit = formatAmount(original.plan.limit_price);
    if (!r.result) {
      retryNote = "Nothing left to retry.";
    } else {
      // Either way this was the one retry: keep it so the button goes away.
      results = [...results, r.result];
      retryNote = r.result.parks.length
        ? `Retried ${r.unfilled_lots} lots within the original limit ${limit} B.`
        : `Nothing is on the book within the original limit ${limit} B; ${r.unfilled_lots} lots stay unfilled.`;
    }
    await store.refresh();
  }
</script>

{#if results.length}
  {@const first = results[0]!}
  <div class="result" aria-live="polite">
    <p>
      <strong>Market {side === "Buy" ? "buy" : "sell"}</strong> within {formatAmount(first.plan.limit_price)} B:
      filled <strong class="num">{filledLots}</strong> lots so far{waiting ? `, ${waiting} park(s) waiting for makers` : ""}.
      Each maker settles on their own, so fills can arrive in parts.
    </p>
    <table>
      <thead><tr><th>Order</th><th class="num">Price</th><th class="num">Lots</th><th>Result</th></tr></thead>
      <tbody>
        {#each results as r, i (i)}
          {#each r.parks as p (b64(p.park))}
            {@const s = settlement(p.park)}
            {@const fill = r.plan.plan.fills.find((f) => sameHash(f.order, p.escrow))}
            <tr>
              <td>…{shortHash(p.escrow)}{i ? " (retry)" : ""}</td>
              <td class="num">{fill ? formatAmount(fill.price_per_lot) : ""}</td>
              <td class="num">{p.lots}</td>
              <td>{!s ? "waiting" : s.filled_lots === p.lots ? "filled" : s.filled_lots === 0 ? "refunded" : `filled ${s.filled_lots}, rest refunded`}</td>
            </tr>
          {/each}
        {/each}
      </tbody>
    </table>
    <p class="hint">
      Planned average {formatAveragePrice(first.plan.total_quote_minor, first.plan.total_lots)} B per A.
      {#if refundedLots}{refundedLots} lots refunded by makers who were already filled.{/if}
      {#if first.plan.plan.shortfall}{first.plan.plan.shortfall} lots were beyond your limit.{/if}
      {#if first.changes.length}The book moved after your preview: {first.changes.length} order(s) changed.{/if}
    </p>
    {#if retryNote}<p class="hint">{retryNote}</p>{/if}
    <div class="actions">
      {#if canRetry}
        <button onclick={retry} title="Once, within the original limit price">Retry remainder ({unfilled} lots)</button>
      {:else if results.length === 1 && unfilled > 0 && waiting > 0}
        <button disabled title="Wait until every maker has settled">Retry remainder</button>
      {/if}
      <button onclick={() => (results = [])}>New market order</button>
    </div>
  </div>
{:else}
  <div class="sides" role="radiogroup" aria-label="Market side">
    {#each ["Buy", "Sell"] as const as s (s)}
      <label class="side" class:selected={side === s}>
        <input type="radio" name="market-side" value={s} bind:group={side} />
        {s} A now
      </label>
    {/each}
  </div>
  <div class="fields">
    <label>
      Amount in
      <select bind:value={by}>
        <option value="lots">A (lots)</option>
        <option value="budget">{payAsset} (budget)</option>
      </select>
    </label>
    {#if by === "lots"}
      <label>Quantity (A) <input inputmode="numeric" class="num" bind:value={quantity} /></label>
    {:else}
      <label>Budget ({payAsset}) <input inputmode="decimal" class="num" bind:value={budget} /></label>
    {/if}
    <label>Max slippage (%) <input inputmode="decimal" class="num" bind:value={slippage} /></label>
  </div>

  {#if plan && plan.plan.filled > 0}
    <table>
      <tbody>
        <tr><td>Orders hit</td><td class="num">{plan.plan.fills.length}</td></tr>
        <tr><td>Lots</td><td class="num">{plan.plan.filled}</td></tr>
        <tr><td>Average price</td><td class="num">{formatAveragePrice(plan.total_quote_minor, plan.total_lots)} B</td></tr>
        <tr><td>Worst price</td><td class="num">{plan.worst_price === null ? "—" : formatAmount(plan.worst_price)} B</td></tr>
        <tr><td>Limit (best {plan.reference_price === null ? "—" : formatAmount(plan.reference_price)})</td><td class="num">{formatAmount(plan.limit_price)} B</td></tr>
        <tr class="total"><td>You pay</td><td class="num">{formatAmount(pay(plan.plan.total_cost))} {payAsset}</td></tr>
        <tr><td>You receive if all fill</td><td class="num">{formatAmount(get(plan.plan.total_receives))} {getAsset}</td></tr>
      </tbody>
    </table>
    {#if plan.plan.shortfall > 0}
      <p class="warn">Only {plan.plan.filled} of {plan.plan.filled + plan.plan.shortfall} lots are within {slippage}% of the best price. The rest is not taken.</p>
    {/if}
    {#if plan.unspent_budget}
      <p class="hint">{formatAmount(plan.unspent_budget)} {payAsset} of the budget stays unspent (whole lots only, within the limit).</p>
    {/if}
  {/if}
  {#if blocked && blocked !== "Previewing…"}<p class="warn">{blocked}</p>{/if}
  <p class="hint">Immediate-or-cancel: never rests on the book. Unfilled parts are refunded by the makers.</p>
  <button class="primary" onclick={confirm} disabled={blocked !== null}>
    Market {buying ? "buy" : "sell"} {plan?.plan.filled ?? ""} A
  </button>
{/if}

<style>
  .sides {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.5rem;
  }
  .side {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.4rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.4rem;
    color: var(--text);
    cursor: pointer;
  }
  .side input {
    width: auto;
  }
  .side.selected {
    border-color: var(--accent);
    font-weight: 600;
  }
  .fields {
    display: grid;
    grid-template-columns: 1fr 1fr 1fr;
    gap: 0.5rem;
    margin: 0.75rem 0;
  }
  .total td {
    font-weight: 600;
  }
  .result table,
  table {
    margin-bottom: 0.5rem;
  }
  .actions {
    display: flex;
    gap: 0.5rem;
  }
</style>
