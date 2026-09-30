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
  await wallet.getByLabel("HF", { exact: true }).fill(b);
  await wallet.getByRole("button", { name: "Get test funds" }).click();
}

test("Alice sells, Bob takes 40, Alice cancels the rest", async ({ browser }) => {
  const [aliceUrl, bobUrl] = urls as [string, string];
  const alice = await (await browser.newContext()).newPage();
  const bob = await (await browser.newContext()).newPage();
  await alice.goto(aliceUrl);
  await bob.goto(bobUrl);

  // 1. Test funds: Alice 100 A, Bob 200 HF.
  await getFunds(alice, "100", "0");
  await getFunds(bob, "0", "200");
  await expect(await balanceRow(alice, "Available")).toContainText("100.00");
  await expect(await balanceRow(bob, "Available")).toContainText("200.00");

  // 2. Alice: sell 100 A at 1.20, 1 h.
  const ticket = region(alice, "Place an order");
  await ticket.getByLabel("Sell A").check();
  await ticket.getByLabel("Price (HF per A)").fill("1.20");
  await ticket.getByLabel("Quantity (A)").fill("100");
  await ticket.getByLabel("Expires").selectOption({ label: "1 h" });
  await expect(ticket).toContainText("You lock 100.00 A");
  await ticket.getByRole("button", { name: "Sell 100 A" }).click();
  await expect(region(alice, "My orders")).toContainText("Open");

  // 3. Bob sees 1.20 · 100 and takes 40.
  const level = region(bob, "Order book").getByRole("button", { name: "Buy from 1 order(s): 100 A at 1.20 HF" });
  await expect(level).toBeVisible();
  await level.click();
  const take = region(bob, "Buy A at 1.20 HF or better");
  await take.getByLabel("Quantity (A)").fill("40");
  await take.getByRole("button", { name: "Park 48.00 HF" }).click();
  // Alice's client settles on the signal (or the next poll). Bob's toast
  // comes from the run's signal, which can arrive before his node sees the
  // run, and fades after 8 s: look for it first, then for the settled park.
  await expect(bob.getByRole("status", { name: "Notifications" })).toContainText("Trade settled: 40 of 40 lots");
  await expect(take).toContainText("filled 40 lots");

  // 4. Alice: +48.00 HF, 60 A still locked; the book shows 1.20 · 60.
  await expect(await balanceRow(alice, "Available")).toContainText("48.00");
  await expect(await balanceRow(alice, "Locked in my orders")).toContainText("60.00");
  await expect(
    region(bob, "Order book").getByRole("button", { name: "Buy from 1 order(s): 60 A at 1.20 HF" }),
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

  // 8. The trade shows in both agents' price data (refreshed on the run signal).
  const trades = region(bob, "Recent trades");
  await expect(trades.getByRole("row", { name: /Buy 1\.20 40/ })).toBeVisible();
  await expect(region(alice, "Recent trades").getByRole("row", { name: /Buy 1\.20 40/ })).toBeVisible();
  const stats = bob.getByLabel("Market stats for A/HF");
  await expect(stats).toContainText("1.20");
  await expect(stats).toContainText("40 lots");
  await expect(region(bob, "Price").getByRole("slider")).toBeVisible();

  // For eyeballing the layout (test-results/ is gitignored).
  await alice.screenshot({ path: "test-results/alice.png", fullPage: true });
  await bob.screenshot({ path: "test-results/bob.png", fullPage: true });
  // One open page per agent: two pages of one agent would race to settle.
  await alice.context().close();
  await bob.context().close();
});

// Continues from the demo's balances: Alice 60 A, Bob 152 HF.
test("market buy within 2% slippage, then its one retry", async ({ browser }) => {
  const [aliceUrl, bobUrl] = urls as [string, string];
  const alice = await (await browser.newContext()).newPage();
  const bob = await (await browser.newContext()).newPage();
  await alice.goto(aliceUrl);
  await bob.goto(bobUrl);

  // Alice rests 10 A at 1.20 and 10 A at 1.25.
  const ticket = region(alice, "Place an order");
  for (const price of ["1.20", "1.25"]) {
    await ticket.getByLabel("Sell A").check();
    await ticket.getByLabel("Price (HF per A)").fill(price);
    await ticket.getByLabel("Quantity (A)").fill("10");
    await ticket.getByRole("button", { name: "Sell 10 A" }).click();
    await expect(region(alice, "Activity")).toContainText(`sell 10 A @ ${price} HF`);
  }
  const book = region(bob, "Order book");
  await expect(book.getByRole("button", { name: "Buy from 1 order(s): 10 A at 1.25 HF" })).toBeVisible();
  await expect(book.getByRole("button", { name: "Buy from 1 order(s): 10 A at 1.20 HF" })).toBeVisible();

  // Bob: market buy 15 A at most 2% above 1.20 (limit 1.23).
  const market = region(bob, "Place an order");
  await market.getByText("Market", { exact: true }).click();
  await market.getByLabel("Buy A now").check();
  await market.getByLabel("Quantity (A)").fill("15");
  await market.getByLabel("Max slippage (%)").fill("2");
  await expect(market).toContainText("Only 10 of 15 lots are within 2% of the best price");
  await expect(market).toContainText("1.2000 HF"); // average
  await market.getByRole("button", { name: "Market buy 10 A" }).click();

  // Alice's page settles on the signal; the result fills in.
  await expect(market).toContainText("filled 10 lots so far");
  await expect(market).toContainText("5 lots were beyond your limit");
  await market.getByRole("button", { name: "Retry remainder (5 lots)" }).click();
  await expect(market).toContainText("Nothing is on the book within the original limit 1.23 HF; 5 lots stay unfilled.");
  await expect(market.getByRole("button", { name: /Retry remainder/ })).toHaveCount(0);

  // The 1.25 level was never touched; Bob got 10 A for 12.00 HF.
  await expect(book.getByRole("button", { name: "Buy from 1 order(s): 10 A at 1.25 HF" })).toBeVisible();
  await expect(await balanceRow(bob, "Available")).toContainText("50.00");
  await expect(await balanceRow(bob, "Available")).toContainText("140.00");

  // Two trades now: the chart and the feed show both.
  await expect(region(bob, "Recent trades").getByRole("row", { name: /Buy 1\.20 10/ })).toBeVisible();
  const chart = region(bob, "Price");
  await chart.getByRole("slider").focus();
  await bob.keyboard.press("ArrowLeft"); // the crosshair, from the keyboard

  await bob.screenshot({ path: "test-results/bob-market.png", fullPage: true });
  await chart.screenshot({ path: "test-results/chart.png" });
  await bob.emulateMedia({ colorScheme: "dark" });
  await chart.screenshot({ path: "test-results/chart-dark.png" });
  await alice.context().close();
  await bob.context().close();
});
