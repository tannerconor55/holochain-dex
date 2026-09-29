<script lang="ts">
  import type { DexStore } from "../lib/dex.svelte";

  let { store }: { store: DexStore } = $props();
</script>

<section class="panel" aria-labelledby="activity-h">
  <h2 id="activity-h">Activity</h2>
  {#if store.activity.length === 0}
    <p class="muted">Order and trade events appear here.</p>
  {:else}
    <ol aria-live="polite">
      {#each store.activity as a (a.id)}
        <li class={a.kind}>
          <time class="muted num">{new Date(a.at).toLocaleTimeString()}</time>
          <span>{a.text}</span>
        </li>
      {/each}
    </ol>
  {/if}
</section>

<style>
  ol {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 0.35rem;
    font-size: 0.85rem;
  }
  li {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.6rem;
    border-left: 3px solid var(--border);
    padding-left: 0.5rem;
  }
  .filled,
  .settled,
  .collected {
    border-color: var(--bid);
  }
  .failed,
  .expired {
    border-color: var(--ask);
  }
  .refunded,
  .cancelled,
  .incoming {
    border-color: var(--warn);
  }
</style>
