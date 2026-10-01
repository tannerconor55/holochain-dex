<script lang="ts">
  // The market's latest trades, newest first. The side is the taker's: a
  // trade against a resting sell was a buy.
  import { b64, shortHash } from "../lib/api";
  import type { DexStore } from "../lib/dex.svelte";
  import { formatTime } from "../lib/format";

  let { store }: { store: DexStore } = $props();
</script>

<section class="panel" aria-labelledby="trades-h">
  <h2 id="trades-h">Recent trades</h2>
  {#if !store.trades}
    <p class="muted">Loading trades…</p>
  {:else if store.trades.length === 0}
    <p class="muted">No trades yet in {store.base}/{store.quote}.</p>
  {:else}
    <table>
      <thead>
        <tr><th>Time</th><th>Side</th><th class="num">Price ({store.quote})</th><th class="num">Lots</th><th>Order</th></tr>
      </thead>
      <tbody>
        {#each store.trades as t (b64(t.run))}
          {@const buy = t.maker_side === "Sell"}
          <tr>
            <td class="muted num">{formatTime(t.timestamp)}</td>
            <td class={buy ? "buy" : "sell"}>{buy ? "Buy" : "Sell"}</td>
            <td class="num">{store.fmt(t.price_per_lot, store.quote)}</td>
            <td class="num">{t.lots}</td>
            <td class="muted">…{shortHash(t.escrow)}</td>
          </tr>
        {/each}
      </tbody>
    </table>
    <p class="hint">Settled trades, derived from the makers' runs. Refreshes on settlement and every 10 s.</p>
  {/if}
</section>

<style>
  .buy {
    color: var(--bid);
  }
  .sell {
    color: var(--ask);
  }
</style>
