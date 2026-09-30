<script lang="ts">
  import type { PriceLevel, Side } from "../lib/api";
  import type { DexStore } from "../lib/dex.svelte";
  import { formatSigned } from "../lib/format";

  let { store, onselect }: { store: DexStore; onselect: (s: { take: Side; price_per_lot: number }) => void } =
    $props();

  // Asks run from the highest price down to the spread; bids from the spread down.
  let asks = $derived(store.book ? [...store.book.asks].reverse() : []);
  let bids = $derived(store.book?.bids ?? []);
  let base = $derived(store.base);
  let quote = $derived(store.quote);
  let price = (minor: number) => store.fmt(minor, quote);
  let signed = (minor: number) => formatSigned(minor, store.decimals(quote));
  let maxLots = $derived(Math.max(1, ...asks.map((l) => l.lots), ...bids.map((l) => l.lots)));
</script>

{#snippet row(level: PriceLevel, kind: "ask" | "bid")}
  <li>
    <button
      class="level {kind}"
      style="--depth: {(level.lots / maxLots) * 100}%"
      onclick={() => onselect({ take: kind === "ask" ? "Buy" : "Sell", price_per_lot: level.price_per_lot })}
      aria-label="{kind === 'ask' ? 'Buy from' : 'Sell to'} {level.orders} order(s): {level.lots} {base} at {price(level.price_per_lot)} {quote}"
    >
      <span class="num price">{price(level.price_per_lot)}</span>
      <span class="num">{level.lots}</span>
      <span class="num muted">{level.orders}</span>
    </button>
  </li>
{/snippet}

<section class="panel" aria-labelledby="book-h">
  <h2 id="book-h">Order book</h2>
  {#if !store.book}
    <p class="muted">Loading the book…</p>
  {:else}
    <div class="head muted">
      <span>Price ({quote} per {base})</span><span class="num">Lots ({base})</span><span class="num">Orders</span>
    </div>
    {#if asks.length === 0 && bids.length === 0}
      <p class="muted empty">No open orders. Place one to start the book.</p>
    {:else}
      <ol class="side" aria-label="Asks: sellers of {base}">
        {#each asks as level (level.price_per_lot)}{@render row(level, "ask")}{/each}
      </ol>
      <div class="spread" role="status">
        {#if store.book.spread === null}
          <span class="muted">Spread —</span>
        {:else if store.book.spread < 0}
          <span class="warn">Crossed book: best bid is {signed(-store.book.spread)} {quote} above best ask</span>
        {:else}
          <span>Spread <span class="num">{signed(store.book.spread)}</span> {quote}</span>
        {/if}
      </div>
      <ol class="side" aria-label="Bids: buyers of {base}">
        {#each bids as level (level.price_per_lot)}{@render row(level, "bid")}{/each}
      </ol>
    {/if}
    <p class="hint">Select a level to take it. Refreshes on activity and every 10 s.</p>
  {/if}
</section>

<style>
  .head,
  .level {
    display: grid;
    grid-template-columns: 1.2fr 1fr 0.7fr;
    gap: 0.5rem;
    font-size: 0.85rem;
  }
  .head {
    padding: 0 0.5rem 0.25rem;
  }
  .side {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  .level {
    width: 100%;
    border: none;
    border-radius: 4px;
    padding: 0.3rem 0.5rem;
    text-align: left;
    background: linear-gradient(to left, var(--bar) var(--depth), transparent var(--depth));
  }
  .level:hover {
    outline: 1px solid var(--border);
  }
  .ask {
    --bar: var(--ask-bar);
  }
  .ask .price {
    color: var(--ask);
    text-align: left;
  }
  .bid {
    --bar: var(--bid-bar);
  }
  .bid .price {
    color: var(--bid);
    text-align: left;
  }
  .spread {
    padding: 0.4rem 0.5rem;
    font-size: 0.85rem;
    border-block: 1px dashed var(--border);
    margin: 0.25rem 0;
  }
  .empty {
    padding: 1rem 0.5rem;
  }
</style>
