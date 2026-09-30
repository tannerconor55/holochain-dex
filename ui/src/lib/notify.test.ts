import { describe, expect, it } from "vitest";
import { noticeKey, Notices, toastMs } from "./notify";

const hash = (byte: number) => new Uint8Array(39).fill(byte);

describe("Notices", () => {
  it("claims a key once", () => {
    const n = new Notices();
    expect(n.claim("a")).toBe(true);
    expect(n.claim("a")).toBe(false);
    expect(n.has("a")).toBe(true);
  });

  it("releases by prefix so an event can notify again", () => {
    const n = new Notices();
    n.claim(noticeKey.failed("Collecting", "timeout"));
    n.claim(noticeKey.failed("Settling my orders", "timeout"));
    n.release(noticeKey.failed("Collecting", ""));
    expect(n.claim(noticeKey.failed("Collecting", "timeout"))).toBe(true);
    expect(n.claim(noticeKey.failed("Settling my orders", "timeout"))).toBe(false);
  });
});

describe("noticeKey", () => {
  it("builds the same key from the same ids, whichever path sees them", () => {
    expect(noticeKey.fill(hash(1))).toBe(noticeKey.fill(new Uint8Array(39).fill(1)));
    expect(noticeKey.fill(hash(1))).not.toBe(noticeKey.fill(hash(2)));
  });

  it("tells each partial fill of an order apart, and each status", () => {
    const e = hash(3);
    const keys = new Set([
      noticeKey.order(e, "Partial", 10),
      noticeKey.order(e, "Partial", 20),
      noticeKey.order(e, "Filled", 40),
      noticeKey.order(e, "Cancelled", 20),
    ]);
    expect(keys.size).toBe(4);
  });
});

describe("toastMs", () => {
  it("fades news, keeps problems longer, keeps errors until dismissed", () => {
    expect(toastMs("success")).toBe(8_000);
    expect(toastMs("warn")).toBe(30_000);
    expect(toastMs("error")).toBeNull();
  });
});
