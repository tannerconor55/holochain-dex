<script lang="ts">
  import { onDestroy } from "svelte";
  import { connect, shortHash } from "./lib/api";
  import { DexStore, message } from "./lib/dex.svelte";
  import Wallet from "./components/Wallet.svelte";
  import OrderBook from "./components/OrderBook.svelte";
  import OrderTicket from "./components/OrderTicket.svelte";
  import TakePanel from "./components/TakePanel.svelte";
  import MyOrders from "./components/MyOrders.svelte";
  import ActivityFeed from "./components/Activity.svelte";
  import type { Side } from "./lib/api";

  let store = $state<DexStore | null>(null);
  let failure = $state<string | null>(null);
  let selection = $state<{ take: Side; price_per_lot: number } | null>(null);

  connect()
    .then((api) => {
      store = new DexStore(api);
      store.start();
    })
    .catch((e) => (failure = message(e)));

  onDestroy(() => store?.stop());
</script>

<header>
  <h1>{store ? `${store.base} / ${store.quote}` : "DEX"}</h1>
  {#if store && (store.config?.markets.length ?? 0) > 1}
    <!-- Hidden until the DNA declares a second market. -->
    <label class="market">
      Market
      <select
        value={store.marketId}
        onchange={(e) => {
          selection = null;
          store?.selectMarket(e.currentTarget.value);
        }}
      >
        {#each store.config?.markets ?? [] as m (m.id)}<option value={m.id}>{m.def.base} / {m.def.quote}</option>{/each}
      </select>
    </label>
  {/if}
  {#if store}
    <span class="muted agent" title="Your agent key">agent …{shortHash(store.api.me)}</span>
    {#if store.pending.length}
      <span class="muted" aria-live="polite">{store.pending[0]}…</span>
    {/if}
  {/if}
</header>

{#if failure}
  <main class="single">
    <section class="panel" role="alert">
      <h2>Could not reach the conductor</h2>
      <p>{failure}</p>
      <p class="hint">
        Run <code>npm start</code> in <code>ui/</code> (two agents via hc-spin), or open this page with
        <code>?admin_port=…&amp;app_port=…</code> against a running sandbox.
      </p>
    </section>
  </main>
{:else if !store}
  <main class="single"><p class="muted">Connecting…</p></main>
{:else}
  {#if store.readError}
    <p class="warn banner" role="status">Some data could not be read and may be stale: {store.readError}</p>
  {/if}
  <main>
    <div class="col">
      <Wallet {store} />
      <OrderTicket {store} />
    </div>
    <div class="col">
      <OrderBook {store} onselect={(s) => (selection = s)} />
      {#if selection}
        <TakePanel {store} {selection} onclose={() => (selection = null)} />
      {/if}
    </div>
    <div class="col">
      <MyOrders {store} />
      <ActivityFeed {store} />
    </div>
  </main>
{/if}

<style>
  header {
    display: flex;
    align-items: baseline;
    gap: 1rem;
    flex-wrap: wrap;
    padding: 1rem 1rem 0;
    max-width: 1400px;
    margin: 0 auto;
  }
  h1 {
    font-size: 1.1rem;
    margin: 0;
  }
  .market {
    display: flex;
    flex-direction: row;
    align-items: baseline;
    gap: 0.4rem;
  }
  .market select {
    width: auto;
  }
  .agent {
    font-family: ui-monospace, monospace;
    font-size: 0.85rem;
  }
  main {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 1rem;
    padding: 1rem;
    max-width: 1400px;
    margin: 0 auto;
  }
  main.single {
    grid-template-columns: minmax(0, 40rem);
    justify-content: center;
  }
  .col {
    display: grid;
    gap: 1rem;
    align-content: start;
    min-width: 0;
  }
  .banner {
    max-width: 1400px;
    margin: 0.75rem auto 0;
    width: calc(100% - 2rem);
  }
  @media (max-width: 1000px) {
    main {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
