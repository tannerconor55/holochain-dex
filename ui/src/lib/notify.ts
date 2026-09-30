// Notifications: one per event, whichever path reports it first.
//
// An event can reach the UI twice: as a signal (fast, may be lost) and as a
// change the next poll sees (slow, never lost). Both paths build the same
// key from stable ids, so the second is dropped. A missed signal therefore
// only delays a notification until the next poll.

import { b64 } from "./api";
import type { ActionHash } from "@holochain/client";
import type { OrderStatus } from "./api";

export type NoticeLevel = "info" | "success" | "warn" | "error";

/** Keys for every notification, from ids both a signal and a poll carry. */
export const noticeKey = {
  /** One of the caller's parks came out of a run (filled or refunded). */
  fill: (park: ActionHash) => `fill:${b64(park)}`,
  /** The caller's order reached `status` with `filled` lots filled. */
  order: (escrow: ActionHash, status: OrderStatus, filled: number) => `order:${b64(escrow)}:${status}:${filled}`,
  incoming: (park: ActionHash) => `incoming:${b64(park)}`,
  reclaimable: (park: ActionHash) => `reclaimable:${b64(park)}`,
  reclaimed: (park: ActionHash) => `reclaimed:${b64(park)}`,
  /** A park waiting on a maker who does not answer. */
  waiting: (park: ActionHash) => `waiting:${b64(park)}`,
  /** A write failing with this message; released when the write next works. */
  failed: (label: string, message: string) => `failed:${label}:${message}`,
};

/** The keys already notified. */
export class Notices {
  private seen = new Set<string>();

  /** True the first time `key` is claimed. */
  claim(key: string): boolean {
    if (this.seen.has(key)) return false;
    this.seen.add(key);
    return true;
  }

  has(key: string): boolean {
    return this.seen.has(key);
  }

  /** Forget every key starting with `prefix`, so the event can notify again. */
  release(prefix: string): void {
    for (const k of this.seen) if (k.startsWith(prefix)) this.seen.delete(k);
  }
}

export interface Toast {
  id: number;
  key: string;
  level: NoticeLevel;
  text: string;
}

/** How long a toast stays: problems stay until read (or long), news fades. */
export function toastMs(level: NoticeLevel): number | null {
  switch (level) {
    case "info":
    case "success":
      return 8_000;
    case "warn":
      return 30_000;
    case "error":
      return null;
  }
}
