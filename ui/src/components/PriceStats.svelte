<script lang="ts">
  // Last price and the market's last 24 hours, as a row of stat tiles.
  // Values use text colours: a price going up is not good or bad.
  import type { DexStore } from "../lib/dex.svelte";
  import { formatChangePercent, formatTime } from "../lib/format";

  let { store }: { store: DexStore } = $props();

  let s = $derived(store.stats);
  let quote = $derived(store.quote);
  let base = $derived(store.base);
  let price = (minor: number | null) => (minor === null ? "—" : store.fmt(minor, quote));
  let change = $derived.by(() => {
    if (!s || s.change_24h === null || s.open_24h === null) return null;
    const c = s.change_24h;
    const arrow = c > 0 ? "▲" : c < 0 ? "▼" : "";
    const sign = c > 0 ? "+" : c < 0 ? "−" : "";
    return {
      arrow,
      text: `${sign}${store.fmt(Math.abs(c), quote)}`,
      percent: formatChangePercent(c, s.open_24h),
      label: c > 0 ? "up" : c < 0 ? "down" : "unchanged",
    };
  });
</script>

<dl class="stats" aria-label="Market stats for {base}/{quote}">
  <div class="tile">
    <dt>Last price</dt>
    <dd class="value">{price(s?.last_price ?? null)} <span class="unit">{quote}</span></dd>
    {#if s?.last_trade_at}<dd class="muted small">at {formatTime(s.last_trade_at)}</dd>{/if}
  </div>
  <div class="tile">
    <dt>24 h change</dt>
    {#if change}
      <dd class="value">
        <span aria-hidden="true">{change.arrow}</span>
        <span class="visually-hidden">{change.label}</span>
        {change.text}
        {#if change.percent}<span class="small muted">({change.percent})</span>{/if}
      </dd>
    {:else}
      <dd class="value muted">—</dd>
    {/if}
  </div>
  <div class="tile">
    <dt>24 h high / low</dt>
    <dd class="value">{price(s?.high_24h ?? null)} / {price(s?.low_24h ?? null)}</dd>
  </div>
  <div class="tile">
    <dt>24 h volume</dt>
    <dd class="value">
      {s?.volume_lots_24h ?? 0} <span class="unit">lots</span>
      <span class="small muted">· {store.fmt(s?.volume_quote_24h ?? 0, quote)} {quote}</span>
    </dd>
    <dd class="muted small">{s?.trades_24h ?? 0} trade{s?.trades_24h === 1 ? "" : "s"}</dd>
  </div>
</dl>

<style>
  .stats {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem 1.5rem;
    margin: 0;
  }
  .tile {
    display: grid;
    gap: 0.05rem;
    min-width: 7rem;
  }
  dt {
    font-size: 0.75rem;
    color: var(--muted);
  }
  dd {
    margin: 0;
  }
  .value {
    font-weight: 600;
    font-size: 1rem;
  }
  .unit,
  .small {
    font-size: 0.75rem;
    font-weight: 400;
  }
  .visually-hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
