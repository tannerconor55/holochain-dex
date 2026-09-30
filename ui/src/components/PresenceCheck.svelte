<script lang="ts">
  // Before a take: ping the makers it would park against, and warn about any
  // who did not answer. Advisory only (dex_api::MakerPresence): parking still
  // works, and the override is one click. `ok` is false while checking and
  // while an unanswered maker is not overridden.
  import type { ActionHash } from "@holochain/client";
  import { untrack } from "svelte";
  import { b64, shortHash, type MakerPresence } from "../lib/api";
  import { message, type DexStore } from "../lib/dex.svelte";

  let { store, orders, ok = $bindable(false) }: { store: DexStore; orders: ActionHash[]; ok?: boolean } = $props();

  let presence = $state<MakerPresence[] | null>(null);
  let failure = $state<string | null>(null);
  let override = $state(false);

  // A new set of orders re-checks; a late answer for an old set is dropped.
  let key = $derived(orders.map(b64).join(","));
  let seq = 0;
  $effect(() => {
    const mine = ++seq;
    const list = key ? untrack(() => orders) : [];
    presence = null;
    failure = null;
    override = false;
    if (!list.length) return;
    store
      .checkMakers(list)
      .then((p) => mine === seq && (presence = p))
      .catch((e) => mine === seq && (failure = message(e)));
  });

  let unanswered = $derived(
    presence?.filter((p, i, all) => !p.reachable && !store.seenRecently(p.maker) && all.findIndex((q) => b64(q.maker) === b64(p.maker)) === i) ?? [],
  );
  let checking = $derived(orders.length > 0 && presence === null && failure === null);
  let waitMinutes = $derived(
    store.config ? Math.ceil((store.config.park_timeout_secs + store.config.settle_grace_secs) / 60) : null,
  );

  $effect(() => {
    ok = orders.length === 0 || override || (!checking && failure === null && unanswered.length === 0);
  });
</script>

{#if checking}
  <p class="hint" aria-live="polite">Checking the maker{orders.length > 1 ? "s are" : " is"} online…</p>
{:else if failure || unanswered.length}
  <div class="warn" role="alert">
    {#if failure}
      <p>Could not check whether the makers are online: {failure}</p>
    {:else}
      <p>
        {unanswered.length === 1 ? "A maker" : `${unanswered.length} makers`} did not answer
        ({unanswered.map((p) => `…${shortHash(p.maker)}`).join(", ")}). Only the maker can settle a park. If they
        stay offline your funds wait {waitMinutes ? `about ${waitMinutes} min` : "until the park's deadline"}, and you can
        reclaim them after that once the maker is back online.
      </p>
    {/if}
    <label class="override"><input type="checkbox" bind:checked={override} /> Park anyway</label>
  </div>
{/if}

<style>
  .warn p {
    margin: 0 0 0.4rem;
  }
  .override {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 0.4rem;
    color: var(--text);
  }
  .override input {
    width: auto;
  }
</style>
