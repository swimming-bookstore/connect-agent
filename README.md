# Connect Agent

Box in an AWS private subnet. Laptop at home. Neither accepts inbound.

The agent **dials** the [control plane](https://github.com/swimming-bookstore/connect-control-plane) (`:4433` TLS) and keeps `Session` open. That stream is the only way the laptop reaches Chromium or a shell.

This repo is the **box agent**. The product plane is [connect-control-plane](https://github.com/swimming-bookstore/connect-control-plane). Local fold: `demo-control-plane`, `demo-turn`, `demo-client`. Not production.

Desktop apps use the same WebRTC as a browser (`RTCPeerConnection` / libwebrtc). Signaling is JSON on `App`; media is ICE. JPEG is the fallback when you do not want RTP (still on `App`, keep frames under the plane 256 KiB cap).

The box coding agent uses the laptop AI share (Grok Login). Tokens stay on the laptop; completions route through demo-client.

| pipe | carries | path |
|---|---|---|
| Plane `App` | open / click / key / offer / answer / ice / jpeg / stdin | both outbound to the plane |
| WebRTC | VP8 RTP | STUN punch first; **TURN** if NAT GW / CGNAT block it |

```
# box
connect-agent --coord plane.example:4433 --tls-ca ca.pem --token TOKEN --key box.key \
  --turn turn:turn.example:3478 --turn-user u --turn-pass p

# laptop (demo)
demo-client --coord plane.example:4433 --tls-ca ca.pem --token CLIENT --key alice.key
# then:  open box-1 browser jpeg
#        go example.com
#        open box-1 shell none
#        stdin ls
```

`--turn` is optional. Without it, ICE is STUN-only. Laptop × private EC2 almost always needs TURN.

`open` picks `kind` (`browser` \| `shell` \| `agent`) and `video` (`webrtc` \| `jpeg` \| `none`). One Chromium, PTY, or coding-agent loop per channel (`--max-sessions` 4). Dies on `close`, peer offline, or `--idle-secs` 180. Browser needs `chromium` (`CHROME=`). WebRTC needs `ffmpeg` (`FFMPEG=`).

Whole fold: see `design.html`. Record the live Chromium window (XGetImage, not CDP): `./scripts/record-fold.sh` → `docs/demo.mp4`.
