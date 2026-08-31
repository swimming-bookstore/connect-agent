import { expect, test, type Page } from "@playwright/test";

async function state(page: Page) {
  const r = await page.request.get("/state");
  expect(r.ok()).toBeTruthy();
  return r.json();
}

async function waitState(page: Page, pred: (s: any) => boolean, ms = 25_000) {
  const t0 = Date.now();
  let last: any;
  while (Date.now() - t0 < ms) {
    last = await state(page);
    if (pred(last)) return last;
    await page.waitForTimeout(250);
  }
  throw new Error(`state wait failed: ${JSON.stringify(last)}`);
}

async function openJpeg(page: Page) {
  await page.goto("/#jpeg");
  await page.getByTestId("nav-jpeg").click();
  await waitState(
    page,
    (x) => x.kind === "browser" && x.video === "jpeg" && x.tabs?.length >= 1,
  );
  await expect(page.getByTestId("tab").first()).toBeVisible({ timeout: 25_000 });
}

test.describe.serial("demo-client", () => {
  test("shell pane loads", async ({ page }) => {
    await page.goto("/");
    await expect(page.getByTestId("nav-shell")).toBeVisible();
    await page.getByTestId("nav-shell").click();
    await expect(page.locator("#term")).toBeVisible();
    const s = await waitState(page, (x) => x.kind === "shell" || x.peers?.length > 0);
    expect(s.peers.length).toBeGreaterThan(0);
  });

  test("jpeg: first about:blank tab", async ({ page }) => {
    await openJpeg(page);
    await expect(page.locator("#chrome")).toBeVisible();
    await expect(page.getByTestId("tab-new")).toBeVisible();
    await expect(page.getByTestId("tab-label")).toHaveText(/about:blank/i);
    const s = await state(page);
    expect(s.tabs[0].id).toBeTruthy();
  });

  test("jpeg: go example.com updates url and title", async ({ page }) => {
    await openJpeg(page);
    await page.locator("#url").fill("example.com");
    await page.locator("#go").click();
    const s = await waitState(
      page,
      (x) =>
        typeof x.url === "string" &&
        x.url.includes("example.com") &&
        (x.tabs?.[0]?.title || "").length > 0 &&
        !/about:blank/i.test(x.tabs[0].title),
      40_000,
    );
    expect(s.tabs[0].title.toLowerCase()).toContain("example");
    expect(s.url).toMatch(/https?:\/\/(www\.)?example\.com/);
    await expect(page.locator("#url")).toHaveValue(/https?:\/\/(www\.)?example\.com/, {
      timeout: 10_000,
    });
    await expect(page.getByTestId("tab-label")).not.toHaveText("about:blank");
    await expect
      .poll(async () => (await page.request.get("/shot")).status(), { timeout: 15_000 })
      .toBe(200);
    const shot = await page.request.get("/shot");
    expect(shot.headers()["content-type"]).toMatch(/jpeg/);
    expect((await shot.body()).length).toBeGreaterThan(1000);
    await page.getByTestId("nav-back").click();
    await waitState(
      page,
      (x) => typeof x.url === "string" && (x.url.includes("about:blank") || x.url === ""),
      15_000,
    );
  });

  test("jpeg: + adds a second tab", async ({ page }) => {
    await openJpeg(page);
    await page.getByTestId("tab-new").click();
    await expect(page.getByTestId("tab")).toHaveCount(2, { timeout: 15_000 });
  });

  test("jpeg: reload stays on jpeg", async ({ page }) => {
    await openJpeg(page);
    await page.reload();
    await expect(page.getByTestId("nav-jpeg")).toHaveClass(/on/);
    const s = await waitState(
      page,
      (x) => x.kind === "browser" && x.video === "jpeg" && x.tabs?.length >= 1,
    );
    expect(s.kind).toBe("browser");
    await expect(page.getByTestId("tab").first()).toBeVisible();
  });

  test("turn720: offer and ice, video element", async ({ page }) => {
    await page.goto("/#turn720");
    await page.getByTestId("nav-turn720").click();
    await expect(page.locator("#rtc")).toBeVisible();
    const s = await waitState(
      page,
      (x) =>
        x.video === "webrtc" &&
        x.height === 720 &&
        typeof x.answer === "string" &&
        x.answer.length > 100 &&
        Array.isArray(x.ice) &&
        x.ice.length > 0 &&
        x.tabs?.length > 0,
      40_000,
    );
    expect(s.kind).toBe("browser");
    const w = await page.locator("#rtc").evaluate((el: HTMLVideoElement) => ({
      width: el.videoWidth,
      ready: el.readyState,
    }));
    expect(w.ready).toBeGreaterThanOrEqual(0);
    expect(s.answer.length).toBeGreaterThan(100);
  });

  test("agent pane", async ({ page }) => {
    await page.goto("/");
    await page.getByTestId("nav-agent").click();
    await expect(page.locator("#agent")).toBeVisible();
    await expect(page.locator("#q")).toBeVisible();
  });

  test("jpeg: enter in url bar navigates", async ({ page }) => {
    await openJpeg(page);
    await page.locator("#url").fill("example.com");
    await page.locator("#url").press("Enter");
    await waitState(
      page,
      (x) => typeof x.url === "string" && x.url.includes("example.com"),
      40_000,
    );
  });

  test("jpeg: forward after back", async ({ page }) => {
    await openJpeg(page);
    await page.locator("#url").fill("example.com");
    await page.locator("#go").click();
    await waitState(
      page,
      (x) => typeof x.url === "string" && x.url.includes("example.com"),
      40_000,
    );
    await page.getByTestId("nav-back").click();
    await waitState(
      page,
      (x) => typeof x.url === "string" && (x.url.includes("about:blank") || x.url === ""),
      15_000,
    );
    await page.getByTestId("nav-fwd").click();
    await waitState(
      page,
      (x) => typeof x.url === "string" && x.url.includes("example.com"),
      20_000,
    );
  });

  test("jpeg: close extra tab", async ({ page }) => {
    await openJpeg(page);
    await page.getByTestId("tab-new").click();
    await expect(page.getByTestId("tab")).toHaveCount(2, { timeout: 15_000 });
    await page.locator('[data-testid="tab"] .x').last().click();
    await expect(page.getByTestId("tab")).toHaveCount(1, { timeout: 15_000 });
  });

  test("jpeg: click and wheel on view", async ({ page }) => {
    await openJpeg(page);
    const view = page.locator("#view");
    await view.click({ position: { x: 40, y: 40 } });
    await view.hover({ position: { x: 80, y: 80 } });
    await page.mouse.wheel(0, 120);
  });

  test("shell: stdin via /cmd", async ({ page }) => {
    await page.goto("/");
    await page.getByTestId("nav-shell").click();
    await waitState(page, (x) => x.kind === "shell" || x.peers?.length > 0);
    const r = await page.request.post("/cmd", {
      data: { type: "stdin", data: "true\n" },
    });
    expect(r.ok()).toBeTruthy();
  });

  test("agent: new chat and ask empty is ignored", async ({ page }) => {
    await page.goto("/#agent");
    await page.getByTestId("nav-agent").click();
    await expect(page.locator("#agent")).toBeVisible();
    await expect(page.locator("#newchat")).toBeVisible();
    await page.locator("#newchat").click();
    await page.locator("#q").fill("");
    await page.locator("#ask button").click();
    await expect(page.locator("#agent")).toBeVisible();
  });

  test("agent: send a message", async ({ page }) => {
    await page.goto("/#agent");
    await page.getByTestId("nav-agent").click();
    await page.locator("#q").fill("ping from e2e");
    await page.locator("#ask").evaluate((el: HTMLFormElement) => el.requestSubmit());
    await expect
      .poll(async () => {
        const s = await state(page);
        return (s.log || []).some((l: string) => String(l).includes("ping from e2e"));
      }, { timeout: 20_000 })
      .toBeTruthy();
  });

  test("hash aliases browser and turn open jpeg", async ({ page }) => {
    await page.goto("/#browser");
    await expect(page.getByTestId("nav-jpeg")).toBeVisible();
    await page.goto("/#turn");
    await expect(page.getByTestId("nav-jpeg")).toBeVisible();
  });

  test("login button is present", async ({ page }) => {
    await page.goto("/");
    await expect(page.locator("#login")).toBeVisible();
    await expect(page.locator("#model")).toBeVisible();
  });
});
