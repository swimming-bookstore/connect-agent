#!/usr/bin/env python3
"""Record docs/demo.mp4 by capturing the live demo-client Chromium window.

Capture is XGetImage of that window only (same idea as fun-coding-agent).
CDP/HTTP only drive the UI; they never become the video.
"""

from __future__ import annotations

import atexit
import ctypes
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from ctypes import (
    CFUNCTYPE,
    POINTER,
    Structure,
    byref,
    c_char_p,
    c_int,
    c_long,
    c_uint,
    c_ulong,
    c_void_p,
    cast,
)
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "target" / "debug"
OUT = ROOT / "docs" / "demo.mp4"
CA = ROOT / "target" / "demo-ca.pem"
FPS = 15
TITLE = "connect-agent-fold-demo"
CDP_PORT = 9223
MAX_SEC = 180.0

os.environ.setdefault("DISPLAY", ":0.0")
os.environ.setdefault("XAUTHORITY", str(Path.home() / ".Xauthority"))

CHILDREN: list[subprocess.Popen] = []

x11 = ctypes.CDLL("libX11.so.6")
xcomp = ctypes.CDLL("libXcomposite.so.1")
xfixes = ctypes.CDLL("libXfixes.so.3")

ZPixmap = 2
AllPlanes = c_ulong(~0)
CompositeRedirectAutomatic = 0


class XImage(Structure):
    _fields_ = [
        ("width", c_int),
        ("height", c_int),
        ("xoffset", c_int),
        ("format", c_int),
        ("data", c_void_p),
        ("byte_order", c_int),
        ("bitmap_unit", c_int),
        ("bitmap_bit_order", c_int),
        ("bitmap_pad", c_int),
        ("depth", c_int),
        ("bytes_per_line", c_int),
        ("bits_per_pixel", c_int),
        ("red_mask", c_ulong),
        ("green_mask", c_ulong),
        ("blue_mask", c_ulong),
        ("obdata", c_void_p),
        ("f", c_void_p * 6),
    ]


x11.XOpenDisplay.restype = c_void_p
x11.XOpenDisplay.argtypes = [c_char_p]
x11.XCloseDisplay.argtypes = [c_void_p]
x11.XDefaultRootWindow.restype = c_ulong
x11.XDefaultRootWindow.argtypes = [c_void_p]
x11.XQueryTree.restype = c_int
x11.XQueryTree.argtypes = [
    c_void_p,
    c_ulong,
    POINTER(c_ulong),
    POINTER(c_ulong),
    POINTER(POINTER(c_ulong)),
    POINTER(c_uint),
]
x11.XFetchName.restype = c_int
x11.XFetchName.argtypes = [c_void_p, c_ulong, POINTER(c_char_p)]
x11.XGetGeometry.restype = c_int
x11.XGetGeometry.argtypes = [
    c_void_p,
    c_ulong,
    POINTER(c_ulong),
    POINTER(c_int),
    POINTER(c_int),
    POINTER(c_uint),
    POINTER(c_uint),
    POINTER(c_uint),
    POINTER(c_uint),
]
x11.XGetImage.restype = POINTER(XImage)
x11.XGetImage.argtypes = [
    c_void_p,
    c_ulong,
    c_int,
    c_int,
    c_uint,
    c_uint,
    c_ulong,
    c_int,
]
x11.XDestroyImage.restype = c_int
x11.XDestroyImage.argtypes = [POINTER(XImage)]
x11.XFree.argtypes = [c_void_p]
x11.XFreePixmap.argtypes = [c_void_p, c_ulong]
x11.XFlush.argtypes = [c_void_p]
x11.XSync.argtypes = [c_void_p, c_int]
x11.XSetErrorHandler.restype = c_void_p
x11.XSetErrorHandler.argtypes = [c_void_p]
x11.XInternAtom.restype = c_ulong
x11.XInternAtom.argtypes = [c_void_p, c_char_p, c_int]
x11.XGetWindowProperty.restype = c_int
x11.XGetWindowProperty.argtypes = [
    c_void_p,
    c_ulong,
    c_ulong,
    c_long,
    c_long,
    c_int,
    c_ulong,
    POINTER(c_ulong),
    POINTER(c_int),
    POINTER(c_ulong),
    POINTER(c_ulong),
    POINTER(c_void_p),
]
x11.XRaiseWindow.argtypes = [c_void_p, c_ulong]
x11.XMapRaised.argtypes = [c_void_p, c_ulong]

xcomp.XCompositeQueryExtension.restype = c_int
xcomp.XCompositeQueryExtension.argtypes = [c_void_p, POINTER(c_int), POINTER(c_int)]
xcomp.XCompositeRedirectWindow.argtypes = [c_void_p, c_ulong, c_int]
xcomp.XCompositeNameWindowPixmap.restype = c_ulong
xcomp.XCompositeNameWindowPixmap.argtypes = [c_void_p, c_ulong]
xfixes.XFixesQueryExtension.restype = c_int
xfixes.XFixesQueryExtension.argtypes = [c_void_p, POINTER(c_int), POINTER(c_int)]
xfixes.XFixesHideCursor.argtypes = [c_void_p, c_ulong]
xfixes.XFixesShowCursor.argtypes = [c_void_p, c_ulong]


@CFUNCTYPE(c_int, c_void_p, c_void_p)
def _xerr(_dpy, _ev):
    return 0


_KEEP_HANDLER = _xerr


def die(msg: str, code: int = 1) -> None:
    print(msg, file=sys.stderr)
    sys.exit(code)


def kill_all() -> None:
    for p in reversed(CHILDREN):
        if p.poll() is None:
            p.send_signal(signal.SIGTERM)
    time.sleep(0.2)
    for p in reversed(CHILDREN):
        if p.poll() is None:
            p.kill()


atexit.register(kill_all)


def spawn(args: list[str], **kw) -> subprocess.Popen:
    kw.setdefault("stdin", subprocess.DEVNULL)
    p = subprocess.Popen(args, cwd=ROOT, **kw)
    CHILDREN.append(p)
    return p


def port_up(host: str, port: int) -> bool:
    s = socket.socket()
    s.settimeout(0.3)
    try:
        s.connect((host, port))
        return True
    except OSError:
        return False
    finally:
        s.close()


def wait_tcp(host: str, port: int, secs: float = 20) -> None:
    deadline = time.time() + secs
    while time.time() < deadline:
        if port_up(host, port):
            return
        time.sleep(0.15)
    die(f"timeout waiting {host}:{port}")


def http_get(url: str, timeout: float = 8) -> tuple[int, bytes, str]:
    req = urllib.request.Request(url, method="GET")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read(), r.headers.get("content-type", "")
    except urllib.error.HTTPError as e:
        return e.code, e.read(), e.headers.get("content-type", "")


def post_cmd(obj: dict) -> None:
    req = urllib.request.Request(
        "http://127.0.0.1:3056/cmd",
        data=json.dumps(obj).encode(),
        method="POST",
        headers={"content-type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=20) as r:
        r.read()


def state() -> dict:
    _, body, _ = http_get("http://127.0.0.1:3056/state")
    return json.loads(body.decode())


def wait_pred(pred, secs: float, label: str) -> dict:
    deadline = time.time() + secs
    last: dict = {}
    while time.time() < deadline:
        last = state()
        if pred(last):
            print(f"  ok {label}")
            return last
        time.sleep(0.25)
    print(
        f"  timeout {label} kind={last.get('kind')} video={last.get('video')} "
        f"h={last.get('height')} url={last.get('url')!r}"
    )
    return last


def children_of(dpy, win: int) -> list[int]:
    root = c_ulong()
    parent = c_ulong()
    kids = POINTER(c_ulong)()
    n = c_uint()
    if not x11.XQueryTree(dpy, win, byref(root), byref(parent), byref(kids), byref(n)):
        return []
    out = [kids[i] for i in range(n.value)]
    if kids:
        x11.XFree(cast(kids, c_void_p))
    return out


def net_wm_name(dpy, win: int) -> str:
    atom = x11.XInternAtom(dpy, b"_NET_WM_NAME", 0)
    utf8 = x11.XInternAtom(dpy, b"UTF8_STRING", 0)
    actual_type = c_ulong()
    actual_fmt = c_int()
    nitems = c_ulong()
    bytes_after = c_ulong()
    prop = c_void_p()
    status = x11.XGetWindowProperty(
        dpy,
        win,
        atom,
        0,
        1024,
        0,
        utf8,
        byref(actual_type),
        byref(actual_fmt),
        byref(nitems),
        byref(bytes_after),
        byref(prop),
    )
    if status != 0 or not prop:
        return ""
    s = ctypes.string_at(prop, nitems.value).decode("utf-8", "replace")
    x11.XFree(prop)
    return s


def window_name(dpy, win: int) -> str:
    n = net_wm_name(dpy, win)
    if n:
        return n
    name = c_char_p()
    if x11.XFetchName(dpy, win, byref(name)) and name.value:
        s = name.value.decode("utf-8", "replace")
        x11.XFree(name)
        return s
    return ""


def walk_windows(dpy, win: int) -> list[int]:
    out = [win]
    for c in children_of(dpy, win):
        out.extend(walk_windows(dpy, c))
    return out


def find_window(dpy, title: str) -> int:
    root = x11.XDefaultRootWindow(dpy)
    needles = (title.lower(), "connectagentfold")
    best = 0
    best_area = 0
    for win in walk_windows(dpy, root):
        name = window_name(dpy, win).lower()
        if not any(n in name for n in needles):
            continue
        w, h = geometry(dpy, win)
        if w < 640 or h < 400:
            continue
        area = w * h
        if area > best_area:
            best = win
            best_area = area
    if best:
        return best
    try:
        out = subprocess.check_output(
            ["xwininfo", "-root", "-tree"],
            text=True,
            stderr=subprocess.DEVNULL,
        )
    except subprocess.CalledProcessError:
        return 0
    for line in out.splitlines():
        low = line.lower()
        if not any(n in low for n in needles):
            continue
        tok = line.strip().split()[0]
        if not tok.startswith("0x"):
            continue
        wid = int(tok, 16)
        w, h = geometry(dpy, wid)
        if w >= 640 and h >= 400:
            return wid
    return 0


def geometry(dpy, win: int) -> tuple[int, int]:
    root = c_ulong()
    x = c_int()
    y = c_int()
    w = c_uint()
    h = c_uint()
    bw = c_uint()
    depth = c_uint()
    if not x11.XGetGeometry(
        dpy, win, byref(root), byref(x), byref(y), byref(w), byref(h), byref(bw), byref(depth)
    ):
        return 0, 0
    return int(w.value), int(h.value)


def grab_bgr(dpy, win: int, w: int, h: int, redirected: bool) -> bytes | None:
    drawable = win
    pix = 0
    if redirected:
        pix = xcomp.XCompositeNameWindowPixmap(dpy, win)
        if pix:
            drawable = pix
    img_p = x11.XGetImage(dpy, drawable, 0, 0, w, h, AllPlanes, ZPixmap)
    if pix:
        x11.XFreePixmap(dpy, pix)
    if not img_p:
        return None
    img = img_p.contents
    if img.bits_per_pixel != 32 or not img.data:
        x11.XDestroyImage(img_p)
        return None
    raw = ctypes.string_at(img.data, img.bytes_per_line * h)
    stride = img.bytes_per_line
    x11.XDestroyImage(img_p)
    row = w * 4
    if stride == row:
        return raw
    packed = bytearray(h * row)
    for y in range(h):
        packed[y * row : (y + 1) * row] = raw[y * stride : y * stride + row]
    return bytes(packed)


def ffmpeg_bin() -> str:
    ff = os.environ.get("FF") or str(Path.home() / ".local/bin/ffmpeg")
    if os.access(ff, os.X_OK):
        return ff
    return shutil.which("ffmpeg") or die("ffmpeg not found")


def boot_fold() -> None:
    spawn([str(BIN / "demo-control-plane"), "--bind", "127.0.0.1:4433", "--ca", str(CA)])
    wait_tcp("127.0.0.1", 4433)
    spawn([str(BIN / "demo-turn"), "--bind", "0.0.0.0:3478", "--user", "u", "--pass", "p"])
    time.sleep(0.3)
    spawn(
        [
            str(BIN / "connect-agent"),
            "--coord",
            "127.0.0.1:4433",
            "--tls-ca",
            str(CA),
            "--token",
            "box-1",
            "--key",
            str(ROOT / "target/box.key"),
            "--idle-secs",
            "600",
            "--turn",
            "turn:127.0.0.1:3478",
            "--turn-user",
            "u",
            "--turn-pass",
            "p",
        ]
    )
    time.sleep(0.5)
    spawn(
        [
            str(BIN / "demo-client"),
            "--coord",
            "127.0.0.1:4433",
            "--tls-ca",
            str(CA),
            "--token",
            "alice",
            "--key",
            str(ROOT / "target/alice.key"),
            "--peer",
            "box-1",
            "--http",
            "127.0.0.1:3056",
            "--turn",
            "turn:127.0.0.1:3478",
            "--turn-user",
            "u",
            "--turn-pass",
            "p",
        ]
    )
    wait_tcp("127.0.0.1", 3056, 25)


class Ws:
    def __init__(self, url: str):
        from urllib.parse import urlparse
        import base64
        import struct

        u = urlparse(url)
        self.sock = socket.create_connection((u.hostname, u.port or 80), timeout=10)
        self.sock.settimeout(10)
        key = base64.b64encode(os.urandom(16)).decode()
        path = u.path or "/"
        if u.query:
            path += "?" + u.query
        req = (
            f"GET {path} HTTP/1.1\r\n"
            f"Host: {u.hostname}:{u.port}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n"
            "\r\n"
        )
        self.sock.sendall(req.encode())
        buf = b""
        while b"\r\n\r\n" not in buf:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise RuntimeError("cdp websocket handshake closed")
            buf += chunk
        if b"101" not in buf.split(b"\r\n", 1)[0]:
            raise RuntimeError(f"cdp handshake failed: {buf[:200]!r}")
        extra = buf.split(b"\r\n\r\n", 1)[1]
        self._rest = extra
        self._id = 0

    def _send_frame(self, payload: bytes) -> None:
        import struct

        mask = os.urandom(4)
        header = bytearray()
        header.append(0x81)
        n = len(payload)
        if n < 126:
            header.append(0x80 | n)
        elif n < 65536:
            header.append(0x80 | 126)
            header.extend(struct.pack("!H", n))
        else:
            header.append(0x80 | 127)
            header.extend(struct.pack("!Q", n))
        header.extend(mask)
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        self.sock.sendall(header + masked)

    def _read_exact(self, n: int) -> bytes:
        out = bytearray()
        if self._rest:
            take = self._rest[:n]
            self._rest = self._rest[n:]
            out.extend(take)
        while len(out) < n:
            chunk = self.sock.recv(n - len(out))
            if not chunk:
                raise RuntimeError("cdp socket closed")
            out.extend(chunk)
        return bytes(out)

    def _recv_frame(self) -> bytes:
        import struct

        while True:
            b1, b2 = self._read_exact(2)
            opcode = b1 & 0x0F
            masked = b2 & 0x80
            n = b2 & 0x7F
            if n == 126:
                n = struct.unpack("!H", self._read_exact(2))[0]
            elif n == 127:
                n = struct.unpack("!Q", self._read_exact(8))[0]
            mask = self._read_exact(4) if masked else b""
            payload = self._read_exact(n)
            if masked:
                payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
            if opcode == 0x8:
                raise RuntimeError("cdp websocket closed")
            if opcode == 0x9:
                self.sock.sendall(bytes([0x8A, 0x80, 0, 0, 0, 0]))
                continue
            if opcode in (0x1, 0x2, 0x0):
                return payload

    def call(self, method: str, params: dict | None = None, timeout: float = 20) -> dict:
        self._id += 1
        mid = self._id
        msg = {"id": mid, "method": method}
        if params:
            msg["params"] = params
        self._send_frame(json.dumps(msg).encode())
        deadline = time.time() + timeout
        while time.time() < deadline:
            self.sock.settimeout(max(0.2, deadline - time.time()))
            try:
                raw = self._recv_frame()
            except socket.timeout:
                continue
            data = json.loads(raw.decode())
            if data.get("id") == mid:
                if "error" in data:
                    raise RuntimeError(f"{method}: {data['error']}")
                return data.get("result") or {}
        raise TimeoutError(method)

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass


def js(ws: Ws, expr: str) -> object:
    r = ws.call(
        "Runtime.evaluate",
        {"expression": expr, "returnByValue": True},
    )
    return (r.get("result") or {}).get("value")


def attach_ui() -> Ws:
    chrome = shutil.which("chromium") or shutil.which("google-chrome") or os.environ.get("CHROME")
    if not chrome:
        die("need chromium")
    udd = ROOT / "target" / "fold-ui-chrome"
    if udd.exists():
        shutil.rmtree(udd, ignore_errors=True)
    spawn(
        [
            chrome,
            "--ozone-platform=x11",
            "--class=ConnectAgentFold",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--autoplay-policy=no-user-gesture-required",
            "--disable-features=WebRtcHideLocalIpsWithMdns",
            "--disable-accelerated-video-decode",
            "--disable-accelerated-video-encode",
            "--no-sandbox",
            f"--remote-debugging-port={CDP_PORT}",
            f"--user-data-dir={udd}",
            "--window-size=1280,800",
            "--window-position=40,40",
            f"--app=http://127.0.0.1:3056/#turn720",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        env={**os.environ, "DISPLAY": os.environ.get("DISPLAY", ":0.0")},
    )
    wait_tcp("127.0.0.1", CDP_PORT, 15)
    page = None
    deadline = time.time() + 10
    while time.time() < deadline and page is None:
        _, body, _ = http_get(f"http://127.0.0.1:{CDP_PORT}/json/list")
        tabs = json.loads(body.decode())
        for t in tabs:
            if t.get("type") == "page" and "3056" in (t.get("url") or ""):
                page = t
                break
        if page is None and tabs:
            page = next((t for t in tabs if t.get("type") == "page"), tabs[0])
        time.sleep(0.2)
    if not page:
        die("no CDP page")
    ws = Ws(page["webSocketDebuggerUrl"])
    ws.call("Page.enable")
    ws.call("Runtime.enable")
    ws.call("Page.bringToFront")
    try:
        win = ws.call("Browser.getWindowForTarget")
        wid = win.get("windowId")
        if wid is not None:
            ws.call(
                "Browser.setWindowBounds",
                {
                    "windowId": wid,
                    "bounds": {
                        "left": 40,
                        "top": 40,
                        "width": 1280,
                        "height": 800,
                        "windowState": "normal",
                    },
                },
            )
    except Exception as e:
        print(f"  window bounds: {e}")
    js(ws, f"document.title = {json.dumps(TITLE)}")
    time.sleep(0.8)
    return ws


def click_tab(ws: Ws, label: str) -> None:
    got = js(
        ws,
        f"""(() => {{
          const b = [...document.querySelectorAll('#nav button')]
            .find(x => (x.textContent || '').trim() === {json.dumps(label)});
          if (!b) return 'missing';
          b.click();
          return (b.textContent || '').trim();
        }})()""",
    )
    print(f"  click {label!r} -> {got!r}")
    if got != label:
        die(f"nav button {label!r} not found ({got!r})")
    time.sleep(0.5)


def focus_sel(ws: Ws, selector: str) -> None:
    got = js(
        ws,
        f"""(() => {{
          const el = document.querySelector({json.dumps(selector)});
          if (!el) return null;
          el.focus();
          el.click();
          const r = el.getBoundingClientRect();
          return {{x: r.left + Math.min(24, r.width / 2), y: r.top + r.height / 2}};
        }})()""",
    )
    if not isinstance(got, dict):
        die(f"no {selector}")
    x, y = float(got["x"]), float(got["y"])
    for typ in ("mousePressed", "mouseReleased"):
        ws.call(
            "Input.dispatchMouseEvent",
            {"type": typ, "x": x, "y": y, "button": "left", "clickCount": 1},
        )
    time.sleep(0.15)


def key_event(ws: Ws, typ: str, **params) -> None:
    ws.call("Input.dispatchKeyEvent", {"type": typ, **params})


def type_keys(ws: Ws, text: str, delay: float = 0.08) -> None:
    for ch in text:
        if ch == "\n":
            key_event(
                ws,
                "keyDown",
                key="Enter",
                code="Enter",
                windowsVirtualKeyCode=13,
            )
            key_event(
                ws,
                "keyUp",
                key="Enter",
                code="Enter",
                windowsVirtualKeyCode=13,
            )
        else:
            key_event(ws, "keyDown", text=ch, unmodifiedText=ch, key=ch)
        time.sleep(delay)


def clear_input(ws: Ws, selector: str) -> None:
    js(
        ws,
        f"""(() => {{
          const i = document.querySelector({json.dumps(selector)});
          if (!i) return;
          i.focus();
          const proto = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value');
          proto.set.call(i, '');
          i.dispatchEvent(new Event('input', {{bubbles: true}}));
        }})()""",
    )


def click_sel(ws: Ws, selector: str) -> None:
    got = js(
        ws,
        f"""(() => {{
          const el = document.querySelector({json.dumps(selector)});
          if (!el) return null;
          const r = el.getBoundingClientRect();
          return {{x: r.left + r.width / 2, y: r.top + r.height / 2}};
        }})()""",
    )
    if not isinstance(got, dict):
        die(f"no {selector}")
    x, y = float(got["x"]), float(got["y"])
    for typ in ("mousePressed", "mouseReleased"):
        ws.call(
            "Input.dispatchMouseEvent",
            {"type": typ, "x": x, "y": y, "button": "left", "clickCount": 1},
        )
    time.sleep(0.2)


def type_input(ws: Ws, selector: str, text: str, submit: bool = True) -> None:
    """Type one char at a time via the native value setter (Leptos on:input)."""
    focus_sel(ws, selector)
    clear_input(ws, selector)
    time.sleep(0.2)
    for ch in text:
        js(
            ws,
            f"""(() => {{
              const i = document.querySelector({json.dumps(selector)});
              if (!i) return;
              const proto = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value');
              proto.set.call(i, (i.value || '') + {json.dumps(ch)});
              i.dispatchEvent(new Event('input', {{bubbles: true}}));
            }})()""",
        )
        time.sleep(0.09)
    time.sleep(0.25)
    if submit:
        if selector == "#q":
            click_sel(ws, "#ask button")
        else:
            type_keys(ws, "\n", delay=0.05)
        time.sleep(0.2)


def go_url(ws: Ws, url: str) -> None:
    shown = url.removeprefix("https://").removeprefix("http://").rstrip("/")
    type_input(ws, "#url", shown, submit=True)
    typed = js(ws, "document.querySelector('#url')?.value || ''")
    print(f"  go {url} ui={typed!r}")


def wait_video(ws: Ws, secs: float = 25) -> None:
    deadline = time.time() + secs
    last = "0x0"
    while time.time() < deadline:
        last = str(
            js(
                ws,
                """(() => {
                  const v = document.querySelector('#rtc');
                  const vw = v ? v.videoWidth : 0;
                  const vh = v ? v.videoHeight : 0;
                  const rs = v ? v.readyState : -1;
                  return vw + 'x' + vh + ' rs=' + rs;
                })()""",
            )
            or "0x0"
        )
        print(f"  video {last}")
        if last.split()[0] not in ("0x0", "None") and not last.startswith("0x"):
            time.sleep(1.2)
            return
        time.sleep(0.5)
    print(f"  video timeout {last}")


def type_agent(ws: Ws, text: str) -> None:
    type_input(ws, "#q", text, submit=True)


def type_shell(ws: Ws, text: str) -> None:
    """Type into xterm so the recording shows keystrokes. xterm posts stdin."""
    js(
        ws,
        """(() => {
          const t = document.querySelector('#term textarea, #term .xterm-helper-textarea');
          if (t) t.focus();
          const term = document.querySelector('#term');
          if (term) term.click();
          return true;
        })()""",
    )
    time.sleep(0.25)
    type_keys(ws, text, delay=0.07)


def hold(secs: float) -> None:
    time.sleep(secs)


def main() -> int:
    x11.XSetErrorHandler(_KEEP_HANDLER)
    ffmpeg = ffmpeg_bin()
    OUT.parent.mkdir(parents=True, exist_ok=True)

    if port_up("127.0.0.1", 3056):
        print("using running demo-client :3056")
    else:
        subprocess.check_call(["cargo", "build", "--bins"], cwd=ROOT)
        boot_fold()

    wait_pred(lambda s: bool(s.get("peers")), 15, "peers")
    ws = attach_ui()

    dpy = x11.XOpenDisplay(None)
    if not dpy:
        die("cannot open X display")

    wid = 0
    for _ in range(80):
        wid = find_window(dpy, TITLE)
        if wid:
            break
        time.sleep(0.15)
    if not wid:
        die("demo window not found")

    ev = c_int()
    er = c_int()
    redirected = bool(xcomp.XCompositeQueryExtension(dpy, byref(ev), byref(er)))
    if redirected:
        xcomp.XCompositeRedirectWindow(dpy, wid, CompositeRedirectAutomatic)
        x11.XSync(dpy, 0)

    root = x11.XDefaultRootWindow(dpy)
    hidden = False
    evb = c_int()
    erb = c_int()
    if xfixes.XFixesQueryExtension(dpy, byref(evb), byref(erb)):
        xfixes.XFixesHideCursor(dpy, root)
        hidden = True

    w, h = geometry(dpy, wid)
    for _ in range(40):
        if w >= 640 and h >= 400:
            break
        time.sleep(0.1)
        w, h = geometry(dpy, wid)
    if w < 640 or h < 400:
        die(f"bad window size {w}x{h}")
    w -= w % 2
    h -= h % 2
    x11.XMapRaised(dpy, wid)
    x11.XRaiseWindow(dpy, wid)
    x11.XFlush(dpy)
    print(f"capturing {w}x{h} window {hex(wid)}", file=sys.stderr)

    ff = subprocess.Popen(
        [
            ffmpeg,
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "bgr0",
            "-s",
            f"{w}x{h}",
            "-r",
            str(FPS),
            "-i",
            "-",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-crf",
            "18",
            "-preset",
            "fast",
            "-movflags",
            "+faststart",
            str(OUT),
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    CHILDREN.append(ff)
    assert ff.stdin is not None

    stop = threading.Event()
    nframes = {"n": 0}

    def pump() -> None:
        last = None
        tick = 1.0 / FPS
        t0 = time.monotonic()
        while not stop.is_set():
            if time.monotonic() - t0 > MAX_SEC:
                break
            frame = grab_bgr(dpy, wid, w, h, redirected)
            if frame is None:
                frame = last
            if frame is None:
                time.sleep(tick)
                continue
            last = frame
            try:
                ff.stdin.write(frame)
            except BrokenPipeError:
                break
            nframes["n"] += 1
            time.sleep(tick)
        try:
            ff.stdin.close()
        except OSError:
            pass

    cap = threading.Thread(target=pump, daemon=True)
    cap.start()

    try:
        print("shell")
        click_tab(ws, "Shell")
        wait_pred(
            lambda s: s.get("kind") == "shell"
            and bool(s.get("stdout"))
            and s.get("last") != "exit 0",
            20,
            "shell",
        )
        hold(1.2)
        type_shell(ws, "echo DEMO-SHELL; uname -s\n")
        wait_pred(lambda s: "DEMO-SHELL" in (s.get("stdout") or ""), 12, "stdout")
        hold(3.5)

        print("jpeg")
        click_tab(ws, "Browser JPEG")
        wait_pred(lambda s: s.get("kind") == "browser" and s.get("video") == "jpeg", 25, "jpeg")
        hold(1.5)
        go_url(ws, "https://example.com/")
        wait_pred(lambda s: "example" in (s.get("url") or "").lower(), 30, "jpeg url")
        hold(4.0)

        print("turn 720p")
        click_tab(ws, "Browser TURN 720p")
        wait_pred(
            lambda s: s.get("kind") == "browser"
            and s.get("video") == "webrtc"
            and int(s.get("height") or 0) in (0, 720)
            and (s.get("answer") or s.get("width")),
            25,
            "turn720",
        )
        hold(1.5)
        go_url(ws, "https://example.com/")
        wait_pred(lambda s: "example" in (s.get("url") or "").lower(), 30, "turn720 url")
        wait_video(ws, 30)
        hold(4.0)

        print("turn 1080p")
        click_tab(ws, "Browser TURN 1080p")
        wait_pred(
            lambda s: s.get("kind") == "browser"
            and s.get("video") == "webrtc"
            and int(s.get("height") or 0) == 1080
            and (s.get("answer") or s.get("width")),
            25,
            "turn1080",
        )
        hold(1.5)
        go_url(ws, "https://example.com/")
        wait_pred(lambda s: "example" in (s.get("url") or "").lower(), 30, "turn1080 url")
        wait_video(ws, 30)
        hold(4.0)

        print("agent")
        click_tab(ws, "Agent")
        wait_pred(lambda s: s.get("kind") == "agent", 20, "agent")
        hold(1.0)
        type_agent(ws, "Reply with exactly: demo-ok")
        wait_pred(
            lambda s: any(
                "demo-ok" in (x or "").lower() or x.startswith("ai ") or x.startswith("error ")
                for x in (s.get("log") or [])
            ),
            20,
            "agent log",
        )
        hold(4.0)
    finally:
        stop.set()
        cap.join(timeout=3)
        ws.close()
        if hidden:
            xfixes.XFixesShowCursor(dpy, root)
            x11.XFlush(dpy)
        x11.XCloseDisplay(dpy)

    err = ff.stderr.read().decode("utf-8", "replace") if ff.stderr else ""
    rc = ff.wait(timeout=30)
    if rc != 0:
        die(f"ffmpeg failed: {err[-800:]}")
    if nframes["n"] < FPS * 8:
        die(f"only {nframes['n']} window frames")
    print(f"wrote {OUT} ({nframes['n']} frames, window capture)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
