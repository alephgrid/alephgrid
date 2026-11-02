// 2.08: leaving a workbook releases everything it registered. The page is instrumented
// before any app code runs: window/document/media-query listeners, ResizeObservers and
// WebSockets are counted, then a workbook is opened and closed repeatedly.

import { expect, test } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

declare global {
  interface Window {
    __leaks: () => { listeners: number; observers: number; sockets: number };
  }
}

test("opening and leaving a workbook does not accumulate listeners, observers or sockets", async ({ page }) => {
  const wb = await server.api<{ id: string }>("/api/workbooks", {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name: "Leak check" }),
  });
  await fetch(`${server.url}/api/workbooks/${wb.id}/import?filename=t.csv`, { method: "POST", body: "a,b\n1,x\n2,y\n" });

  await page.addInitScript(() => {
    let listeners = 0;
    const watched = (t: EventTarget) => t === window || t === document || t instanceof MediaQueryList;
    const add = EventTarget.prototype.addEventListener;
    const remove = EventTarget.prototype.removeEventListener;
    const live = new WeakMap<object, Set<string>>();
    EventTarget.prototype.addEventListener = function (type: string, fn: any, opts?: any) {
      if (watched(this) && fn) {
        const capture = typeof opts === "boolean" ? opts : !!opts?.capture;
        const key = `${type}|${capture}`;
        const keys = live.get(fn) ?? new Set<string>();
        if (!keys.has(key)) {
          keys.add(key);
          live.set(fn, keys);
          listeners++;
          opts?.signal?.addEventListener?.("abort", () => {
            if (keys.delete(key)) listeners--;
          });
        }
      }
      return add.call(this, type, fn, opts);
    };
    EventTarget.prototype.removeEventListener = function (type: string, fn: any, opts?: any) {
      if (watched(this) && fn) {
        const capture = typeof opts === "boolean" ? opts : !!opts?.capture;
        if (live.get(fn)?.delete(`${type}|${capture}`)) listeners--;
      }
      return remove.call(this, type, fn, opts);
    };
    let observers = 0;
    const RO = window.ResizeObserver;
    window.ResizeObserver = class extends RO {
      private on = false;
      observe(t: Element, o?: ResizeObserverOptions) {
        if (!this.on) { this.on = true; observers++; }
        super.observe(t, o);
      }
      disconnect() {
        if (this.on) { this.on = false; observers--; }
        super.disconnect();
      }
    };
    const sockets = new Set<WebSocket>();
    const WS = window.WebSocket;
    window.WebSocket = class extends WS {
      constructor(url: string | URL, p?: string | string[]) {
        super(url, p);
        sockets.add(this);
      }
    } as typeof WebSocket;
    window.__leaks = () => ({
      listeners,
      observers,
      sockets: [...sockets].filter((s) => s.readyState !== WS.CLOSED).length,
    });
  });

  await page.goto(server.url);
  await expect(page.locator(".wb-row", { hasText: "Leak check" })).toBeVisible();
  const baseline = await page.evaluate(() => window.__leaks());

  for (let i = 0; i < 6; i++) {
    await page.locator(".wb-name", { hasText: "Leak check" }).click();
    await expect(page.getByRole("tab", { name: /t/ })).toBeVisible();
    await expect(page.locator(".status-dot.status-open")).toBeVisible();
    // Interact so the grid draws, fetches and registers its handlers.
    await page.locator(".grid-host").focus();
    await page.keyboard.press("ArrowDown");
    // Menus closed by Escape and by choosing an item must not leave listeners behind.
    await page.getByRole("button", { name: "New table or sheet" }).click();
    await page.keyboard.press("Escape");
    await page.getByRole("button", { name: /Export/ }).click();
    await page.getByRole("menuitem").first().press("Escape");
    // The Functions panel listens for window resizes while open; leaving must remove that.
    await page.getByRole("button", { name: "Functions", exact: true }).click();
    await expect(page.getByRole("complementary", { name: "Functions" })).toBeVisible();
    await page.locator("button.brand").click();
    await expect(page.locator(".wb-row", { hasText: "Leak check" })).toBeVisible();
  }

  await expect.poll(() => page.evaluate(() => window.__leaks())).toEqual({ ...baseline, sockets: 0 });
});
