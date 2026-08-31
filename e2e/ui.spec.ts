import { expect, test } from "@playwright/test";

test.describe("demo-client ui chrome", () => {
  test("rail has every pane button", async ({ page }) => {
    await page.goto("/");
    await expect(page.getByTestId("nav-shell")).toHaveText("Shell");
    await expect(page.getByTestId("nav-jpeg")).toHaveText("Browser JPEG");
    await expect(page.getByTestId("nav-turn720")).toHaveText("Browser TURN 720p");
    await expect(page.getByTestId("nav-turn1080")).toHaveText("Browser TURN 1080p");
    await expect(page.getByTestId("nav-agent")).toHaveText("Agent");
  });

  test("switching panes toggles on class", async ({ page }) => {
    await page.goto("/");
    await page.getByTestId("nav-agent").click();
    await expect(page.getByTestId("nav-agent")).toHaveClass(/on/);
    await expect(page.locator("#agent")).not.toHaveClass(/off/);
    await expect(page.locator("#term")).toHaveClass(/off/);
    await page.getByTestId("nav-shell").click();
    await expect(page.getByTestId("nav-shell")).toHaveClass(/on/);
    await expect(page.locator("#term")).not.toHaveClass(/off/);
  });

  test("jpeg chrome chrome bar is hidden on shell", async ({ page }) => {
    await page.goto("/");
    await page.getByTestId("nav-shell").click();
    await expect(page.locator("#chrome")).toHaveClass(/off/);
    await page.getByTestId("nav-jpeg").click();
    await expect(page.locator("#chrome")).not.toHaveClass(/off/);
    await expect(page.locator("#back")).toBeVisible();
    await expect(page.locator("#fwd")).toBeVisible();
    await expect(page.locator("#go")).toBeVisible();
    await expect(page.locator("#url")).toBeVisible();
  });

  test("turn1080 pane shows video, hides jpeg stage", async ({ page }) => {
    await page.goto("/#turn1080");
    await page.getByTestId("nav-turn1080").click();
    await expect(page.getByTestId("nav-turn1080")).toHaveClass(/on/);
    await expect(page.locator("#live")).not.toHaveClass(/off/);
    await expect(page.locator("#rtc")).toBeVisible();
    await expect(page.locator("#stage")).toHaveClass(/off/);
  });

  test("agent placeholder and send", async ({ page }) => {
    await page.goto("/#agent");
    await page.getByTestId("nav-agent").click();
    await expect(page.locator("#q")).toHaveAttribute(
      "placeholder",
      "Message the box coding agent",
    );
    await expect(page.locator("#ask button")).toHaveText("Send");
    await expect(page.locator("#newchat")).toHaveText("New chat");
  });

  test("boot splash is gone after wasm", async ({ page }) => {
    await page.goto("/");
    await expect(page.getByTestId("nav-shell")).toBeVisible();
    await expect(page.locator("#boot")).toHaveCount(0);
  });

  test("hash jpeg marks jpeg nav on", async ({ page }) => {
    await page.goto("/#jpeg");
    await expect(page.getByTestId("nav-jpeg")).toHaveClass(/on/);
  });

  test("unknown hash still renders shell rail", async ({ page }) => {
    await page.goto("/#not-a-pane");
    await expect(page.getByTestId("nav-shell")).toBeVisible();
  });

  test("static assets", async ({ page }) => {
    for (const [path, type] of [
      ["/xterm.css", "text/css"],
      ["/xterm.js", "javascript"],
      ["/xterm-addon-fit.js", "javascript"],
      ["/pkg/demo_client_ui.js", "javascript"],
    ] as const) {
      const r = await page.request.get(path);
      expect(r.ok(), path).toBeTruthy();
      expect(r.headers()["content-type"] || "").toMatch(new RegExp(type));
    }
    const wasm = await page.request.get("/pkg/demo_client_ui_bg.wasm");
    expect(wasm.ok()).toBeTruthy();
    expect(wasm.headers()["content-type"]).toMatch(/wasm/);
  });

  test("404 unknown path", async ({ page }) => {
    const r = await page.request.get("/no-such");
    expect(r.status()).toBe(404);
  });

  test("post cmd open without dst is ok", async ({ page }) => {
    const r = await page.request.post("/cmd", { data: { type: "open" } });
    expect(r.ok()).toBeTruthy();
    expect(await r.text()).toBe("ok");
  });

  test("state json shape", async ({ page }) => {
    const r = await page.request.get("/state");
    expect(r.ok()).toBeTruthy();
    const s = await r.json();
    expect(s).toHaveProperty("peers");
    expect(s).toHaveProperty("kind");
    expect(s).toHaveProperty("video");
    expect(s).toHaveProperty("tabs");
    expect(s).toHaveProperty("log");
    expect(s).toHaveProperty("grok");
    expect(s.grok).toHaveProperty("configured");
    expect(s.grok).toHaveProperty("model");
  });
});
