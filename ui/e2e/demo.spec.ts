// The PLAN.md demo script (§30) between two agents, each in its own tab,
// against two sandboxed conductors started by ../scripts/sandbox.sh.

import { expect, test, type Page } from "@playwright/test";
import { spawn, type ChildProcess } from "node:child_process";
import { createInterface } from "node:readline";

let sandbox: ChildProcess;
let urls: string[] = [];

test.beforeAll(async () => {
  test.setTimeout(3 * 60_000);
  sandbox = spawn("../scripts/sandbox.sh", { detached: true, stdio: ["ignore", "pipe", "inherit"] });
  const lines = createInterface({ input: sandbox.stdout! });
  urls = await new Promise<string[]>((resolve, reject) => {
    const found: string[] = [];
    lines.on("line", (line) => {
      const m = /agent ready: (\S+)/.exec(line);
      if (m?.[1]) found.push(m[1]);
      if (found.length === 2) resolve(found.sort());
    });
    sandbox.on("exit", (code) => reject(new Error(`sandbox exited early (${code})`)));
  });
});

test.afterAll(() => {
  if (sandbox?.pid) process.kill(-sandbox.pid, "SIGINT");
});

const region = (page: Page, name: string | RegExp) => page.getByRole("region", { name });

async function balanceRow(page: Page, row: string) {
  return region(page, "Wallet").getByRole("row", { name: new RegExp(`^${row}`) });
}

async function getFunds(page: Page, a: string, b: string) {
  const wallet = region(page, "Wallet");
  await wallet.getByLabel("A", { exact: true }).fill(a);
  await wallet.getByLabel("B", { exact: true }).fill(b);
  await wallet.getByRole("button", { name: "Get test funds" }).click();
}

test("Alice sells, Bob takes 40, Alice cancels the rest", async ({ browser }) => {
  const [aliceUrl, bobUrl] = urls as [string, string];
  const alice = await (await browser.newContext()).newPage();
  const bob = await (await browser.newContext()).newPage();
  await alice.goto(aliceUrl);
  await bob.goto(bobUrl);

  // 1. Test funds: Alice 100 A, Bob 200 B.
  await getFunds(alice, "100", "0");
  await getFunds(bob, "0", "200");
  await expect(await balanceRow(alice, "Available")).toContainText("100.00");
  await expect(await balanceRow(bob, "Available")).toContainText("200.00");

  // 2. Alice: sell 100 A at 1.20, 1 h.
  const ticket = region(alice, "Place an order");
  await ticket.getByLabel("Sell A").check();
  await ticket.getByLabel("Price (B per A)").fill("1.20");
  await ticket.getByLabel("Quantity (A)").fill("100");
  await ticket.getByLabel("Expires").selectOption({ label: "1 h" });
  await expect(ticket).toContainText("You lock 100.00 A");
  await ticket.getByRole("button", { name: "Sell 100 A" }).click();
  await expect(region(alice, "My orders")).toContainText("Open");

  // 3. Bob sees 1.20 · 100 and takes 40.
  const level = region(bob, "Order book").getByRole("button", { name: "Buy from 1 order(s): 100 A at 1.20 B" });
  await expect(level).toBeVisible();
  await level.click();
  const take = region(bob, "Buy A at 1.20 B or better");
  await take.getByLabel("Quantity (A)").fill("40");
  await take.getByRole("button", { name: "Park 48.00 B" }).click();
  // Alice's client settles on the signal (or the next poll).
  await expect(take).toContainText("filled 40 lots");

  // 4. Alice: +48.00 B, 60 A still locked; the book shows 1.20 · 60.
  await expect(await balanceRow(alice, "Available")).toContainText("48.00");
  await expect(await balanceRow(alice, "Locked in my orders")).toContainText("60.00");
  await expect(
    region(bob, "Order book").getByRole("button", { name: "Buy from 1 order(s): 60 A at 1.20 B" }),
  ).toBeVisible();
  await expect(await balanceRow(bob, "Available")).toContainText("40.00");

  // 5. Alice cancels: 60 A returns and the level disappears.
  await region(alice, "My orders").getByRole("button", { name: "Cancel" }).click();
  await expect(region(alice, "My orders")).toContainText("Cancelled");
  await expect(await balanceRow(alice, "Available")).toContainText("60.00");
  await expect(region(bob, "Order book")).toContainText("No open orders");

  // 7. Both activity feeds narrate it.
  await expect(region(alice, "Activity")).toContainText("Order partially filled: 40/100 lots");
  await expect(region(alice, "Activity")).toContainText("Order cancelled");
  await expect(region(bob, "Activity")).toContainText("Trade settled: 40 of 40 lots");

  // For eyeballing the layout (test-results/ is gitignored).
  await alice.screenshot({ path: "test-results/alice.png", fullPage: true });
  await bob.screenshot({ path: "test-results/bob.png", fullPage: true });
});
