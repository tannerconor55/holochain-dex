<script lang="ts">
  import { amounts, amt, HUB, UNIT_A, type Amounts, type UnitId } from "../lib/api";
  import type { DexStore } from "../lib/dex.svelte";
  import { parseAmount } from "../lib/format";

  let { store }: { store: DexStore } = $props();

  /** dex_core::MAX_MINT: the most one faucet mint may create per asset. */
  const MAX_MINT = 100_000_000;

  /** Every unit the declared markets use, base units first, then the hub. */
  let units = $derived.by((): UnitId[] => {
    const markets = store.config?.markets ?? [];
    if (!markets.length) return [UNIT_A, HUB];
    const seen = [...markets.map((m) => m.def.base), ...markets.map((m) => m.def.quote)];
    return seen.filter((u, i) => seen.indexOf(u) === i);
  });
  /** Faucet inputs by unit; a unit not typed into mints nothing. */
  let faucet = $state<Record<UnitId, string>>({ [UNIT_A]: "100", [HUB]: "200" });

  const rows: [string, keyof NonNullable<typeof store.balance>][] = [
    ["Available", "available"],
    ["Locked in my orders", "locked_in_escrows"],
    ["Parked in takes", "parked"],
    ["Uncollected", "uncollected"],
  ];

  const sum = (xs: Amounts[], unit: string) => xs.reduce((s, x) => s + amt(x, unit), 0);
  let adds = $derived.by(() => {
    const v = store.balance;
    if (!v) return true;
    const rows = [v.available, v.locked_in_escrows, v.parked, v.uncollected];
    return units.every((u) => sum(rows, u) === amt(v.total, u));
  });

  let mint = $derived.by((): { amounts: Amounts } | { reason: string } => {
    const parsed: [UnitId, number][] = [];
    for (const u of units) {
      const minor = parseAmount(faucet[u] || "0", store.decimals(u));
      if (minor === null) return { reason: `${u} takes at most ${store.decimals(u)} decimals.` };
      if (minor > MAX_MINT) return { reason: `At most ${store.fmt(MAX_MINT, u)} ${u} per request.` };
      parsed.push([u, minor]);
    }
    if (parsed.every(([, n]) => n === 0)) return { reason: `Enter an amount of ${units.join(", ")}.` };
    return { amounts: amounts(parsed) };
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
        <tr><th></th>{#each units as u (u)}<th class="num">{u}</th>{/each}</tr>
      </thead>
      <tbody>
        {#each rows as [label, key] (key)}
          <tr>
            <td>{label}</td>
            {#each units as u (u)}<td class="num">{store.fmt(amt(store.balance[key], u), u)}</td>{/each}
          </tr>
        {/each}
        <tr class="total">
          <td>Total</td>
          {#each units as u (u)}<td class="num">{store.fmt(amt(store.balance.total, u), u)}</td>{/each}
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
    {#each units as u (u)}
      <label>{u} <input inputmode="decimal" bind:value={faucet[u]} aria-describedby="faucet-hint" /></label>
    {/each}
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
    grid-template-columns: repeat(auto-fit, minmax(5rem, 1fr));
    gap: 0.5rem;
    align-items: end;
    margin-top: 1rem;
  }
</style>
