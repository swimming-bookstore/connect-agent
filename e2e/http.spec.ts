import { expect, test } from "@playwright/test";

test.describe("demo-client http api", () => {
  test("shot is jpeg or empty", async ({ request }) => {
    const r = await request.get("/shot");
    expect([200, 204]).toContain(r.status());
    if (r.status() === 200) {
      expect(r.headers()["content-type"]).toMatch(/jpeg/);
    }
  });

  test("cmd navigate and tabs", async ({ request }) => {
    const open = await request.post("/cmd", {
      data: { type: "open", dst: "box-1", kind: "browser", video: "jpeg", height: 720 },
    });
    expect(open.ok()).toBeTruthy();
    const nav = await request.post("/cmd", {
      data: { type: "navigate", url: "about:blank" },
    });
    expect(nav.ok()).toBeTruthy();
    const tab = await request.post("/cmd", {
      data: { type: "new_tab", url: "about:blank" },
    });
    expect(tab.ok()).toBeTruthy();
    const back = await request.post("/cmd", { data: { type: "back" } });
    expect(back.ok()).toBeTruthy();
    const fwd = await request.post("/cmd", { data: { type: "forward" } });
    expect(fwd.ok()).toBeTruthy();
  });

  test("cmd click key wheel resize", async ({ request }) => {
    for (const body of [
      { type: "click", x: 10, y: 20 },
      { type: "key", key: "a", pressed: true },
      { type: "key", key: "a", pressed: false },
      { type: "wheel", x: 1, y: 2, deltaX: 0, deltaY: 40 },
      { type: "resize", cols: 80, rows: 24 },
      { type: "stdin", data: "echo\n" },
      { type: "focus", id: "missing" },
      { type: "close_tab", id: "missing" },
      { type: "new_chat" },
    ]) {
      const r = await request.post("/cmd", { data: body });
      expect(r.ok(), JSON.stringify(body)).toBeTruthy();
    }
  });

  test("cmd ask and logout", async ({ request }) => {
    const ask = await request.post("/cmd", { data: { type: "ask", text: " " } });
    expect(ask.ok()).toBeTruthy();
    const logout = await request.post("/cmd", { data: { type: "logout" } });
    expect(logout.ok()).toBeTruthy();
  });

  test("cmd garbage is still 200", async ({ request }) => {
    const r = await request.post("/cmd", { data: { type: "not-a-command" } });
    expect(r.status()).toBe(200);
  });
});
