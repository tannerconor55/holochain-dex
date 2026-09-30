<script lang="ts">
  import { amounts, amt, HUB, UNIT_A, type Amounts } from "../lib/api";
  import type { DexStore } from "../lib/dex.svelte";
  import { formatAmount, parseAmount } from "../lib/format";

  let { store }: { store: DexStore } = $props();

  /** dex_core::MAX_MINT: the most one faucet mint may create per asset. */
  const MAX_MINT = 100_000_000;

  let a = $state("100");
  let b = $state("200");

  const rows: [string, keyof NonNullable<typeof store.balance>][] = [
    ["Available", "available"],
    ["Locked in my orders", "locked_in_escrows"],
    ["Parked in takes", "parked"],
    ["Uncollected", "uncollected"],
  ];

  const UNITS = [UNIT_A, HUB];
  const sum = (xs: Amounts[], unit: string) => xs.reduce((s, x) => s + amt(x, unit), 0);
  let adds = $derived.by(() => {
    const v = store.balance;
    if (!v) return true;
    const rows = [v.available, v.locked_in_escrows, v.parked, v.uncollected];
    return UNITS.every((u) => sum(rows, u) === amt(v.total, u));
  });

  let mint = $derived.by((): { amounts: Amounts } | { reason: string } => {
    const ma = parseAmount(a || "0");
    const mb = parseAmount(b || "0");
    if (ma === null || mb === null) return { reason: "Amounts need at most two decimals." };
    if (ma === 0 && mb === 0) return { reason: "Enter an amount of A, HF or both." };
    if (ma > MAX_MINT || mb > MAX_MINT) return { reason: "At most 1,000,000.00 of each per request." };
    return { amounts: amounts([[UNIT_A, ma], [HUB, mb]]) };
  });

  async function getFunds(e: SubmitEvent) {
    e.preventDefault();
    if (!("amounts" in mint)) return;
    const amounts = mint.amounts;
    const done = await store.write("Minting test funds", () => store.api.mint(amounts));
    if (done) await store.refresh();
  }
</script>

<section class="panel" aria-labelledby="wallet-h">
  <h2 id="wallet-h">Wallet</h2>
  {#if store.balance}
    <table>
      <thead>
        <tr><th></th>{#each UNITS as u (u)}<th class="num">{u}</th>{/each}</tr>
      </thead>
      <tbody>
        {#each rows as [label, key] (key)}
          <tr>
            <td>{label}</td>
            {#each UNITS as u (u)}<td class="num">{formatAmount(amt(store.balance[key], u))}</td>{/each}
          </tr>
        {/each}
        <tr class="total">
          <td>Total</td>
          {#each UNITS as u (u)}<td class="num">{formatAmount(amt(store.balance.total, u))}</td>{/each}
        </tr>
      </tbody>
    </table>
    <p class="hint">
      {#if adds}
        Available + locked + parked + uncollected = total.
      {:else}
        <span class="warn">The rows do not add up to the total; the view may be mid-update.</span>
      {/if}
      Uncollected funds are collected automatically.
    </p>
  {:else}
    <p class="muted">Loading balance…</p>
  {/if}

  <form class="faucet" onsubmit={getFunds}>
    <label>A <input inputmode="decimal" bind:value={a} aria-describedby="faucet-hint" /></label>
    <label>HF <input inputmode="decimal" bind:value={b} aria-describedby="faucet-hint" /></label>
    <button type="submit" disabled={!("amounts" in mint)}>Get test funds</button>
  </form>
  <p class="hint" id="faucet-hint">
    {"reason" in mint ? mint.reason : "Test network only: mints the amounts above to you."}
  </p>
</section>

<style>
  .total td {
    font-weight: 600;
  }
  .faucet {
    display: grid;
    grid-template-columns: 1fr 1fr auto;
    gap: 0.5rem;
    align-items: end;
    margin-top: 1rem;
  }
</style>
