<script lang="ts">
  // Price over time: one series (each candle's close), so no legend; the
  // title names it. Line 2px over a 10% wash, an end dot with a 2px surface
  // ring and the last price as its label, hairline grid. A crosshair snaps
  // to the nearest candle (pointer or arrow keys) and shows its OHLC; the
  // same values are in the table view below, so the tooltip never gates.
  import { CHART_RANGES, INTERVAL_MS, type ChartRange, type DexStore } from "../lib/dex.svelte";
  import { microsToDate } from "../lib/format";

  let { store }: { store: DexStore } = $props();

  const PLOT_H = 160;
  const AXIS_H = 22;
  const PAD = { top: 12, left: 48, right: 64 };

  let width = $state(480);
  let active = $state<number | null>(null);

  let range = $derived(CHART_RANGES[store.chartRange]);
  let candles = $derived(store.candles ?? []);
  let quote = $derived(store.quote);
  let base = $derived(store.base);
  let win = $derived(store.candleWindow);
  let half = $derived((INTERVAL_MS[range.interval] * 1000) / 2);

  let plotW = $derived(Math.max(60, width - PAD.left - PAD.right));
  let x = $derived.by(() => {
    const w = win ?? { from: 0, to: 1 };
    return (t: number) => PAD.left + ((t - w.from) / (w.to - w.from)) * plotW;
  });

  /** A clean step for about `n` ticks over `span` minor units: 1, 2 or 5 × 10^k. */
  function niceStep(span: number, n: number): number {
    const raw = Math.max(1, span / n);
    const mag = 10 ** Math.floor(Math.log10(raw));
    const step = [1, 2, 5, 10].map((m) => m * mag).find((s) => s >= raw) ?? 10 * mag;
    return Math.max(1, Math.round(step));
  }

  let scale = $derived.by(() => {
    if (!candles.length) return null;
    let lo = Math.min(...candles.map((c) => c.low));
    let hi = Math.max(...candles.map((c) => c.high));
    if (lo === hi) {
      const pad = Math.max(1, Math.round(lo * 0.01));
      lo = Math.max(0, lo - pad);
      hi = hi + pad;
    }
    const step = niceStep(hi - lo, 3);
    const min = Math.floor(lo / step) * step;
    const max = Math.ceil(hi / step) * step;
    const ticks: number[] = [];
    for (let v = min; v <= max; v += step) ticks.push(v);
    const y = (v: number) => PAD.top + (1 - (v - min) / (max - min)) * PLOT_H;
    return { min, max, ticks, y };
  });

  let points = $derived(scale ? candles.map((c) => ({ c, px: x(c.start + half), py: scale.y(c.close) })) : []);
  let line = $derived(points.map((p, i) => `${i ? "L" : "M"}${p.px.toFixed(1)},${p.py.toFixed(1)}`).join(""));
  let area = $derived(
    points.length > 1
      ? `${line}L${points[points.length - 1]!.px.toFixed(1)},${PAD.top + PLOT_H}L${points[0]!.px.toFixed(1)},${PAD.top + PLOT_H}Z`
      : "",
  );
  let last = $derived(points[points.length - 1] ?? null);

  /** Round local times per range, thinned to at least `MIN_TICK_PX` apart. */
  const TICK_STEP_MS: Record<ChartRange, number> = { "1h": 15 * 60_000, "24h": 6 * 3_600_000, "7d": 86_400_000 };
  const MIN_TICK_PX = 90;
  let xTicks = $derived.by(() => {
    if (!win) return [];
    const step = TICK_STEP_MS[store.chartRange];
    const offset = new Date().getTimezoneOffset() * 60_000; // align to local time
    const [from, to] = [win.from / 1000, win.to / 1000];
    const ticks: number[] = [];
    for (let t = Math.ceil((from - offset) / step) * step + offset; t < to; t += step) ticks.push(t * 1000);
    const every = Math.max(1, Math.ceil(MIN_TICK_PX / ((step * 1000 * plotW) / (win.to - win.from))));
    return ticks.filter((_, i) => i % every === 0);
  });

  function timeLabel(micros: number): string {
    const d = microsToDate(micros);
    return store.chartRange === "7d"
      ? d.toLocaleDateString([], { weekday: "short", day: "numeric" })
      : d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  }

  function candleTime(micros: number): string {
    const d = microsToDate(micros);
    return d.toLocaleString([], { weekday: "short", hour: "2-digit", minute: "2-digit" });
  }

  function nearest(clientX: number, rect: DOMRect): number | null {
    if (!points.length) return null;
    const px = clientX - rect.left;
    let best = 0;
    for (let i = 1; i < points.length; i++) {
      if (Math.abs(points[i]!.px - px) < Math.abs(points[best]!.px - px)) best = i;
    }
    return best;
  }

  function onKey(e: KeyboardEvent) {
    if (!points.length) return;
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      const start = active ?? points.length - 1;
      active = Math.min(points.length - 1, Math.max(0, start + (e.key === "ArrowLeft" ? -1 : 1)));
    } else if (e.key === "Escape") {
      active = null;
    }
  }

  let hover = $derived(active !== null ? (points[active] ?? null) : null);
  let price = (v: number) => store.fmt(v, quote);
</script>

<section class="panel" aria-labelledby="chart-h">
  <div class="title">
    <h2 id="chart-h">Price</h2>
    <div class="ranges" role="radiogroup" aria-label="Chart range">
      {#each Object.entries(CHART_RANGES) as [key, r] (key)}
        <label class:selected={store.chartRange === key}>
          <input
            type="radio"
            name="chart-range"
            value={key}
            checked={store.chartRange === key}
            onchange={() => store.setChartRange(key as ChartRange)}
          />{r.label}
        </label>
      {/each}
    </div>
  </div>
  <p class="hint">{quote} per {base}, the close of each {range.interval.slice(1)}{range.interval.startsWith("M") ? " min" : " h"}.</p>

  {#if !store.candles}
    <p class="muted">Loading prices…</p>
  {:else if !candles.length}
    <p class="muted">
      No trades in the last {range.label}{store.stats?.last_price != null
        ? `; the last traded at ${price(store.stats.last_price)} ${quote}`
        : ""}.
    </p>
  {:else if scale}
    <div class="frame" class:loading={store.priceLoading} bind:clientWidth={width}>
      <svg
        width={width}
        height={PAD.top + PLOT_H + AXIS_H}
        role="slider"
        tabindex="0"
        aria-label="Price line, {candles.length} intervals, last {price(last?.c.close ?? 0)} {quote}. Arrow keys step through them."
        aria-valuemin={0}
        aria-valuemax={candles.length - 1}
        aria-valuenow={active ?? candles.length - 1}
        aria-valuetext={`${price((hover ?? last)?.c.close ?? 0)} ${quote} at ${candleTime((hover ?? last)?.c.start ?? 0)}`}
        onkeydown={onKey}
        onblur={() => (active = null)}
      >
        {#each scale.ticks as t (t)}
          <line class="grid" x1={PAD.left} x2={PAD.left + plotW} y1={scale.y(t)} y2={scale.y(t)} />
          <text class="tick" x={PAD.left - 6} y={scale.y(t)} text-anchor="end" dominant-baseline="middle">{price(t)}</text>
        {/each}
        {#each xTicks as t (t)}
          <text
            class="tick"
            x={x(t)}
            y={PAD.top + PLOT_H + 15}
            text-anchor="middle">{timeLabel(t)}</text
          >
          <line class="grid" x1={x(t)} x2={x(t)} y1={PAD.top + PLOT_H} y2={PAD.top + PLOT_H + 4} />
        {/each}
        {#if area}<path class="wash" d={area} />{/if}
        <path class="line" d={line} />
        {#if hover}
          <line class="crosshair" x1={hover.px} x2={hover.px} y1={PAD.top} y2={PAD.top + PLOT_H} />
          <circle class="dot" cx={hover.px} cy={hover.py} r="4" />
        {/if}
        {#if last}
          <circle class="dot" cx={last.px} cy={last.py} r="4" />
          <text class="end" x={last.px + 8} y={last.py} dominant-baseline="middle">{price(last.c.close)}</text>
        {/if}
        <rect
          class="hit"
          x={PAD.left}
          y={PAD.top}
          width={plotW}
          height={PLOT_H}
          role="presentation"
          onpointermove={(e) => (active = nearest(e.clientX, (e.currentTarget as SVGRectElement).ownerSVGElement!.getBoundingClientRect()))}
          onpointerleave={() => (active = null)}
        />
      </svg>
      {#if hover}
        <div
          class="tooltip"
          style={hover.px > PAD.left + plotW / 2
            ? `right: ${width - hover.px + 12}px; top: 0`
            : `left: ${hover.px + 12}px; top: 0`}
          aria-live="polite"
        >
          <strong class="num">{price(hover.c.close)} {quote}</strong>
          <span class="muted">{candleTime(hover.c.start)}</span>
          <span class="muted num">O {price(hover.c.open)} · H {price(hover.c.high)} · L {price(hover.c.low)}</span>
          <span class="muted">{hover.c.volume_lots} lots in {hover.c.trades} trade{hover.c.trades === 1 ? "" : "s"}</span>
        </div>
      {/if}
    </div>
    <details>
      <summary>Show as table</summary>
      <table>
        <thead>
          <tr><th>Interval</th><th class="num">Open</th><th class="num">High</th><th class="num">Low</th><th class="num">Close</th><th class="num">Lots</th></tr>
        </thead>
        <tbody>
          {#each [...candles].reverse() as c (c.start)}
            <tr>
              <td>{candleTime(c.start)}</td>
              <td class="num">{price(c.open)}</td>
              <td class="num">{price(c.high)}</td>
              <td class="num">{price(c.low)}</td>
              <td class="num">{price(c.close)}</td>
              <td class="num">{c.volume_lots}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </details>
  {/if}
</section>

<style>
  .title {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 0.5rem;
  }
  .ranges {
    display: flex;
    border: 1px solid var(--border);
    border-radius: 6px;
    overflow: hidden;
  }
  .ranges label {
    display: flex;
    flex-direction: row;
    padding: 0.15rem 0.6rem;
    color: var(--text);
    cursor: pointer;
    font-size: 0.8rem;
  }
  .ranges input {
    position: absolute;
    opacity: 0;
    width: 1px;
  }
  .ranges label.selected {
    background: var(--accent);
    color: var(--accent-text);
    font-weight: 600;
  }
  .ranges label:focus-within {
    outline: 2px solid var(--focus);
    outline-offset: -2px;
  }
  .frame {
    position: relative;
    transition: opacity 0.2s;
  }
  .frame.loading {
    opacity: 0.6;
  }
  svg {
    display: block;
    overflow: visible;
  }
  svg:focus-visible {
    outline: 2px solid var(--focus);
    outline-offset: 2px;
  }
  .grid {
    stroke: var(--border);
    stroke-width: 1;
  }
  .tick {
    fill: var(--muted);
    font-size: 0.7rem;
    font-variant-numeric: tabular-nums;
  }
  .wash {
    fill: var(--series-1-wash);
  }
  .line {
    fill: none;
    stroke: var(--series-1);
    stroke-width: 2;
    stroke-linejoin: round;
    stroke-linecap: round;
  }
  .dot {
    fill: var(--series-1);
    stroke: var(--panel);
    stroke-width: 2;
  }
  .end {
    fill: var(--text);
    font-size: 0.75rem;
    font-weight: 600;
  }
  .crosshair {
    stroke: var(--muted);
    stroke-width: 1;
  }
  .hit {
    fill: transparent;
    cursor: crosshair;
  }
  .tooltip {
    position: absolute;
    display: grid;
    gap: 0.1rem;
    min-width: 150px;
    padding: 0.4rem 0.55rem;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    box-shadow: 0 2px 8px rgb(0 0 0 / 0.12);
    font-size: 0.75rem;
    pointer-events: none;
  }
  .tooltip strong {
    font-size: 0.9rem;
  }
  details {
    margin-top: 0.5rem;
    font-size: 0.8rem;
  }
  details table {
    margin-top: 0.4rem;
  }
</style>
