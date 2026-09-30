<script lang="ts">
  // Notifications as toasts, newest at the bottom. Each is also in the
  // activity log, so dismissing (or missing) one loses nothing.
  import type { DexStore } from "../lib/dex.svelte";

  let { store }: { store: DexStore } = $props();
</script>

<div class="toasts" role="status" aria-live="polite" aria-label="Notifications">
  {#each store.toasts as t (t.id)}
    <div class="toast {t.level}">
      <span>{t.text}</span>
      <button onclick={() => store.dismiss(t.id)} aria-label="Dismiss notification">×</button>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    right: 1rem;
    bottom: 1rem;
    display: grid;
    gap: 0.5rem;
    width: min(24rem, calc(100vw - 2rem));
    z-index: 10;
  }
  .toast {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 0.5rem;
    align-items: start;
    padding: 0.6rem 0.75rem;
    background: var(--panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-left: 4px solid var(--accent);
    border-radius: var(--radius);
    box-shadow: 0 4px 14px rgb(0 0 0 / 0.15);
    font-size: 0.85rem;
  }
  .toast.success {
    border-left-color: var(--bid);
  }
  .toast.warn {
    border-left-color: var(--warn);
  }
  .toast.error {
    border-left-color: var(--ask);
  }
  button {
    border: none;
    background: none;
    color: var(--muted);
    font-size: 1rem;
    line-height: 1;
    padding: 0 0.2rem;
    cursor: pointer;
  }
</style>
