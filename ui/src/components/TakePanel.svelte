<script lang="ts">
  import { amt, b64, HUB, sameHash, shortHash, UNIT_A, type Side, type TakePlan, type TakeResult } from "../lib/api";
  import { message, type DexStore } from "../lib/dex.svelte";
  import { formatAmount, parseAmount, parseLots } from "../lib/format";

  let {
    store,
    selection,
    onclose,
  }: { store: DexStore; selection: { take: Side; price_per_lot: number }; onclose: () => void } = $props();

  let quantity = $state("");
  let limit = $state("");
  let plan = $state<TakePlan | null>(null);
  let planError = $state<string | null>(null);
  let result = $state<TakeResult | null>(null);

  // A new selection resets the panel, with the level's price as the limit.
  $effect(() => {
    limit = formatAmount(selection.price_per_lot);
    quantity = "";
    result = null;
  });

  let buying = $derived(selection.take === "Buy");
  let payAsset = $derived(buying ? HUB : UNIT_A);
  let getAsset = $derived(buying ? UNIT_A : HUB);
  let lots = $derived(parseLots(quantity));
  let limitPrice = $derived(parseAmount(limit));

  // Preview the plan, debounced; a late response for an old input is dropped.
  let seq = 0;
  $effect(() => {
    const request = lots !== null && limitPrice !== null ? { take: selection.take, lots, limit_price: limitPrice } : null;
    const mine = ++seq;
    plan = null;
    planError = null;
    if (!request) return;
    const timer = setTimeout(() => {
      store.api
        .planTake(request)
        .then((p) => mine === seq && (plan = p))
        .catch((e) => mine === seq && (planError = message(e)));
    }, 300);
    return () => clearTimeout(timer);
  });

  let cost = $derived(plan ? amt(plan.total_cost, payAsset) : 0);
  let available = $derived(store.balance ? amt(store.balance.available, payAsset) : null);
  let blocked = $derived.by(() => {
    if (!plan) return "Enter a quantity to see what you would take.";
    if (plan.filled === 0) return "Nothing to take at or better than your limit (your own orders are skipped).";
    if (available !== null && cost > available) {
      return `This costs ${formatAmount(cost)} ${payAsset}; you have ${formatAmount(available)} ${payAsset} available.`;
    }
    return null;
  });

  async function confirm() {
    if (lots === null || limitPrice === null || blocked) return;
    const request = { take: selection.take, lots, limit_price: limitPrice };
    const taken = await store.write("Taking", () => store.api.take(request));
    if (taken) {
      result = taken;
      await store.refresh();
    }
  }

  function parkState(park: Uint8Array): string {
    const status = store.parks?.find((p) => sameHash(p.park, park));
    if (!status?.settlement) return "waiting for the maker";
    return status.settlement.filled_lots === 0 ? "refunded" : `filled ${status.settlement.filled_lots} lots`;
  }
</script>

<section class="panel" aria-labelledby="take-h">
  <div class="title">
    <h2 id="take-h">{buying ? "Buy" : "Sell"} A at {formatAmount(selection.price_per_lot)} HF or better</h2>
    <button onclick={onclose} aria-label="Close the take panel">Close</button>
  </div>

  {#if result}
    <p>
      Parked {formatAmount(amt(result.plan.total_cost, payAsset))} {payAsset} across
      {result.parks.length} order(s). Each maker settles automatically while their app is open.
    </p>
    <table>
      <thead><tr><th>Order</th><th class="num">Lots</th><th>Status</th></tr></thead>
      <tbody>
        {#each result.parks as p (b64(p.park))}
          <tr><td>…{shortHash(p.escrow)}</td><td class="num">{p.lots}</td><td>{parkState(p.park)}</td></tr>
        {/each}
      </tbody>
    </table>
    <button onclick={() => (result = null)}>Take more</button>
  {:else}
    <div class="fields">
      <label>Quantity (A) <input inputmode="numeric" class="num" bind:value={quantity} /></label>
      <label>Limit (B per A) <input inputmode="decimal" class="num" bind:value={limit} /></label>
    </div>
    {#if planError}<p class="warn">Could not plan: {planError}</p>{/if}
    {#if plan && plan.fills.length}
      <table>
        <thead>
          <tr><th>Order</th><th class="num">Price</th><th class="num">Lots</th><th class="num">You pay</th></tr>
        </thead>
        <tbody>
          {#each plan.fills as f (b64(f.order))}
            <tr>
              <td>…{shortHash(f.order)}</td>
              <td class="num">{formatAmount(f.price_per_lot)}</td>
              <td class="num">{f.lots}</td>
              <td class="num">{formatAmount(amt(f.cost, payAsset))} {payAsset}</td>
            </tr>
          {/each}
          <tr class="total">
            <td>Total</td>
            <td></td>
            <td class="num">{plan.filled}</td>
            <td class="num">{formatAmount(cost)} {payAsset}</td>
          </tr>
        </tbody>
      </table>
      <p class="hint">
        You receive {formatAmount(amt(plan.total_receives, getAsset))}
        {getAsset} if every maker fills. Anything a maker cannot fill is refunded.
      </p>
    {/if}
    {#if plan && plan.shortfall > 0 && plan.filled > 0}
      <p class="warn">Only {plan.filled} of {lots} lots are available at this limit.</p>
    {/if}
    {#if blocked}<p class="hint">{blocked}</p>{/if}
    <button class="primary" onclick={confirm} disabled={blocked !== null}>
      Park {plan ? formatAmount(cost) : ""} {payAsset}
    </button>
  {/if}
</section>

<style>
  .title {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 0.5rem;
  }
  .fields {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.5rem;
    margin-bottom: 0.75rem;
  }
  .total td {
    font-weight: 600;
  }
  table {
    margin-bottom: 0.5rem;
  }
</style>
