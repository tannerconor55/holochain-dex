<script lang="ts">
  import { amt, HUB, UNIT_A, type Side } from "../lib/api";
  import type { DexStore } from "../lib/dex.svelte";
  import { formatAmount, nowMicros, parseAmount, parseLots } from "../lib/format";
  import MarketForm from "./MarketForm.svelte";

  let { store }: { store: DexStore } = $props();

  const EXPIRIES = [
    { label: "1 min", micros: 60e6 },
    { label: "15 min", micros: 15 * 60e6 },
    { label: "1 h", micros: 3600e6 },
    { label: "24 h", micros: 24 * 3600e6 },
  ];

  let mode = $state<"limit" | "market">("limit");
  let side = $state<Side>("Sell");
  let price = $state("1.20");
  let quantity = $state("100");
  let expiry = $state(2);

  /** The lock the maker commits, in the asset they deliver. */
  let ticket = $derived.by(() => {
    const pricePerLot = parseAmount(price);
    const lots = parseLots(quantity);
    if (pricePerLot === null || pricePerLot === 0) return { reason: "Enter a price above 0 with at most two decimals." };
    if (lots === null) return { reason: "Enter a whole number of A (1 lot = 1.00 A)." };
    const lock = side === "Sell" ? lots * 100 : lots * pricePerLot;
    const asset = side === "Sell" ? UNIT_A : HUB;
    if (!Number.isSafeInteger(lock)) return { reason: "That order is too large." };
    const available = store.balance ? amt(store.balance.available, asset) : null;
    if (available !== null && lock > available) {
      return { reason: `You lock ${formatAmount(lock)} ${asset} but have ${formatAmount(available)} ${asset} available.`, lock, asset };
    }
    return { pricePerLot, lots, lock, asset };
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (!("pricePerLot" in ticket) || ticket.pricePerLot === undefined) return;
    const terms = {
      side,
      price_per_lot: ticket.pricePerLot,
      lots: ticket.lots,
      expires_at: nowMicros() + EXPIRIES[expiry]!.micros,
    };
    const escrow = await store.write("Placing order", () => store.api.placeOrder(terms));
    if (escrow) await store.refresh();
  }
</script>

<section class="panel" aria-labelledby="ticket-h">
  <div class="title">
    <h2 id="ticket-h">Place an order</h2>
    <div class="modes" role="radiogroup" aria-label="Order type">
      <label class:selected={mode === "limit"}><input type="radio" name="mode" value="limit" bind:group={mode} />Limit</label>
      <label class:selected={mode === "market"}><input type="radio" name="mode" value="market" bind:group={mode} />Market</label>
    </div>
  </div>
  {#if mode === "market"}
    <MarketForm {store} />
  {:else}
  <form onsubmit={submit}>
    <div class="sides" role="radiogroup" aria-label="Side">
      {#each ["Sell", "Buy"] as const as s (s)}
        <label class="side" class:selected={side === s}>
          <input type="radio" name="side" value={s} bind:group={side} />
          {s} A
        </label>
      {/each}
    </div>
    <div class="fields">
      <label>Price (HF per A) <input inputmode="decimal" class="num" bind:value={price} /></label>
      <label>Quantity (A) <input inputmode="numeric" class="num" bind:value={quantity} /></label>
      <label>
        Expires
        <select bind:value={expiry}>
          {#each EXPIRIES as e, i (e.label)}<option value={i}>{e.label}</option>{/each}
        </select>
      </label>
    </div>
    <p class="preview" aria-live="polite">
      {#if "lock" in ticket && ticket.lock !== undefined}
        You lock <strong class="num">{formatAmount(ticket.lock)} {ticket.asset}</strong> until the order fills, is
        cancelled or expires.
      {/if}
    </p>
    {#if "reason" in ticket}<p class="warn">{ticket.reason}</p>{/if}
    <button class="primary" type="submit" disabled={!("pricePerLot" in ticket)}>
      {side} {parseLots(quantity) ?? ""} A
    </button>
  </form>
  {/if}
</section>

<style>
  .title {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 0.5rem;
  }
  .modes {
    display: flex;
    border: 1px solid var(--border);
    border-radius: 6px;
    overflow: hidden;
  }
  .modes label {
    flex-direction: row;
    display: flex;
    align-items: center;
    padding: 0.2rem 0.7rem;
    color: var(--text);
    cursor: pointer;
  }
  .modes input {
    position: absolute;
    opacity: 0;
    width: 1px;
  }
  .modes label.selected {
    background: var(--accent);
    color: var(--accent-text);
    font-weight: 600;
  }
  .modes label:focus-within {
    outline: 2px solid var(--focus);
    outline-offset: -2px;
  }
  form {
    display: grid;
    gap: 0.75rem;
  }
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
  }
  .preview {
    margin: 0;
    min-height: 1.4em;
  }
</style>
