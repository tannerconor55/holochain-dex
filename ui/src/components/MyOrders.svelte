<script lang="ts">
  import { b64, shortHash, type MyOrder } from "../lib/api";
  import { formatPair, type DexStore } from "../lib/dex.svelte";
  import { formatAmount, formatExpiry, formatTime } from "../lib/format";

  let { store }: { store: DexStore } = $props();

  const live = (o: MyOrder) => o.status === "Open" || o.status === "Partial";
  let open = $derived((store.orders ?? []).filter((o) => live(o) || (o.status === "Expired" && !o.state.closed)));
  let history = $derived((store.orders ?? []).filter((o) => !open.includes(o)));
  let waiting = $derived(open.reduce((n, o) => n + o.pending.length, 0));
  let cancelling = $derived(store.pending.includes("Cancelling order"));

  async function cancel(o: MyOrder) {
    const reports = await store.write("Cancelling order", () => store.api.cancelOrder(o.state.escrow));
    if (reports) await store.refresh();
  }
</script>

{#snippet orderRow(o: MyOrder, withCancel: boolean)}
  <tr>
    <td>{o.state.terms.side === "Sell" ? "Sell" : "Buy"} …{shortHash(o.state.escrow)}</td>
    <td class="num">{formatAmount(o.state.terms.price_per_lot)}</td>
    <td class="num">{o.state.filled_lots}/{o.state.terms.lots}</td>
    <td>
      {o.status}
      {#if o.pending.length}<span class="badge">{o.pending.length} waiting</span>{/if}
    </td>
    <td class="muted">{live(o) ? formatExpiry(o.state.terms.expires_at) : ""}</td>
    {#if withCancel}
      <td>
        <button
          onclick={() => cancel(o)}
          disabled={cancelling || o.state.closed}
          title={o.state.closed ? "Already released" : "Release the remaining lock and refund waiting takers"}
        >Cancel</button>
      </td>
    {/if}
  </tr>
{/snippet}

<section class="panel" aria-labelledby="mine-h">
  <h2 id="mine-h">My orders</h2>
  {#if !store.orders}
    <p class="muted">Loading your orders…</p>
  {:else if store.orders.length === 0}
    <p class="muted">You have not placed any orders.</p>
  {:else}
    {#if open.length}
      <table>
        <thead>
          <tr><th>Order</th><th class="num">Price</th><th class="num">Filled</th><th>Status</th><th>Expires</th><th></th></tr>
        </thead>
        <tbody>{#each open as o (b64(o.state.escrow))}{@render orderRow(o, true)}{/each}</tbody>
      </table>
      <p class="hint">
        {#if waiting}{waiting} take(s) waiting: settling now. {/if}Your orders settle automatically only while this
        app is open; takers wait for you otherwise.
      </p>
    {/if}
    {#if history.length}
      <h3>History</h3>
      <table>
        <tbody>{#each history as o (b64(o.state.escrow))}{@render orderRow(o, false)}{/each}</tbody>
      </table>
    {/if}
  {/if}

  {#if store.parks?.length}
    <h3>My takes</h3>
    <table>
      <thead><tr><th>Order</th><th class="num">Parked</th><th>Result</th><th>At</th></tr></thead>
      <tbody>
        {#each store.parks as p (b64(p.park))}
          <tr>
            <td>…{shortHash(p.escrow)}</td>
            <td class="num">{formatPair(p.amounts)}</td>
            <td>
              {#if !p.settlement}waiting for the maker{:else if p.settlement.filled_lots === 0}refunded{:else}filled
                {p.settlement.filled_lots}/{p.requested_lots}{/if}
            </td>
            <td class="muted">{formatTime(p.parked_at)}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</section>

<style>
  h3 {
    font-size: 0.85rem;
    margin: 1rem 0 0.4rem;
    color: var(--muted);
  }
  .badge {
    font-size: 0.75rem;
    background: var(--warn-bg);
    color: var(--warn);
    border-radius: 999px;
    padding: 0 0.4rem;
    margin-left: 0.25rem;
  }
</style>
