# Session: guest view and MCP

A paid `desktop.linux` lease starts an isolated Docker guest (`berthos-linux-desktop:v1`, `--network none`). The buyer gets a `leaseId`, an occupancy receipt, and — on this node — a **loopback-only view of that guest** plus an MCP stdio server that can screenshot and drive it.

This repo does **not** take payment. Wallets, x402, listings, and the market UI live in [berth-market](https://github.com/hexuria/berth-market). After the market has paid, the buyer talks to **this** node on `127.0.0.1`.

Isolation is the product. View and MCP target the **guest** Xvfb (`DISPLAY=:99` inside the container). They never read the host `DISPLAY`, host `/tmp/.X11-unix`, or the host cursor.

## Roles

| Role | What they run | What they see |
| --- | --- | --- |
| **Operator / host** | `berth node up` on a VM or dedicated server | Pairing code, loopback HTTP. Not a rented desktop. |
| **Buyer / agent** | After market pay: `berth pair`, `berth up --os linux`, then `berth view` / `berth mcp` | The isolated Linux guest only |

A personal laptop is still rejected as a public node (`class=laptop`). A laptop chassis may host a **private** loopback node that still leases a guest.

## Pairing token (lease bearer)

`POST /v1/pair` exchanges the one-time code for a bearer token. The default pair grants `operator` and `lease`.

View and MCP require the **`lease`** capability:

```
Authorization: Bearer <token>
```

The raw token is shown once at pair time and stored in `~/.berthos/client.toml` (mode `0600`). The node stores only `SHA-256(token)`.

The loopback HTML viewer also accepts `?token=<token>` so a browser on the same machine can open the page without an extension. That query is loopback-only; do not put the token on a tunnel URL.

The guest never sees the token. `~/.berthos` is never mounted into the container.

## Operator: park the node

```sh
docker build -t berthos-linux-desktop:v1 images/linux-desktop
berth doctor                 # fail closed
berth node up                # refuses bind-all; prints pairing code
# pairing code: ABCD-EFGH
# listening on http://127.0.0.1:7432
```

The node HTTP stays on `127.0.0.1`. There is no market UI here.

## Buyer: lease, view, MCP

Payments happen in berth-market, not this process. Locally you can still exercise the same node APIs:

```sh
berth pair --code ABCD-EFGH
# token stored in ~/.berthos/client.toml (mode 0600)

berth up --os linux
# lease l_…
# viewer http://127.0.0.1:<ephemeral>/

berth view
# prints http://127.0.0.1:<ephemeral>/?token=<lease-bearer>
# open that URL on this machine — GUEST Xvfb, not the host desktop

# agent (stdio JSON-RPC)
berth mcp
# or: claude mcp add --transport stdio berth -- berth mcp
```

`berth view` is node-local. It prints the per-lease loopback viewer. That listener is **not** `0.0.0.0`. It dies when the lease ends.

```sh
berth end
# occupancy receipt (seconds). View port is gone. Guest is destroyed.
```

## What the view is

After `POST /v1/leases` the node binds `127.0.0.1:0` and serves:

| Path | Role |
| --- | --- |
| `GET /` | Guest desktop page (screenshot stream + click/type). Banner says GUEST. |
| `GET /screenshot` | PNG of the guest Xvfb via `docker exec … /usr/local/bin/driver screenshot` |
| `POST /action` | `click` / `type` / `key` inside the guest |
| `GET /vnc.html` | noVNC from the guest image, if present |
| `GET /websockify` | WebSocket pipe to guest `x11vnc` on **guest** `127.0.0.1:5900` (`docker exec socat`) |

The guest itself has `--network none`. Host port publish is not used. The node reaches guest `:5900` only through `docker exec`.

`GET /v1/leases/{id}/view` on the node (lease bearer) returns `{ viewer_url, target: "guest", token: "…" }`.

## MCP tools

`berth mcp` is stdio JSON-RPC. Tools use the lease bearer from `client.toml` and `GET /v1/leases`. If that list is empty they **refuse**.

| Tool | Effect |
| --- | --- |
| `berth_screenshot` | PNG of the live guest. Never the host display. |
| `berth_click` | Click in the guest (`x`, `y`, optional `button`) |
| `berth_type` | Type into the guest |
| `berth_key` | Key / chord in the guest |
| `berth_end` | `DELETE /v1/leases/{id}` — guest + view gone |

The image ships `/usr/local/bin/driver` (`action.sh`: xdotool + ImageMagick `import` against guest `:99`).

## HTTP (loopback)

In addition to the existing lease API:

| Method | Path | Auth | Notes |
| --- | --- | --- | --- |
| `GET` | `/v1/leases/{id}/screenshot` | lease | Guest PNG. `404` if no live lease. |
| `POST` | `/v1/leases/{id}/actions` | lease | `{ "op": "click"\|"type"\|"key", … }`. Guest only. |
| `GET` | `/v1/leases/{id}/view` | lease | `{ viewer_url }` for this lease |
| `DELETE` | `/v1/leases/{id}` | lease | Ends lease; view listener is dropped |

`POST /v1/leases` includes `viewer_url` (`http://127.0.0.1:<port>/`) while the lease is live.

## Reproduce (two roles, no payment)

This is the honest local path. Nothing is charged.

**Terminal A — operator**

```sh
docker build -t berthos-linux-desktop:v1 images/linux-desktop
cargo install --path crates/berthos-cli
berth doctor
berth node up
```

**Terminal B — buyer / agent** (same machine; after a market pay you would pair the same way)

```sh
berth pair --code <code from A>
berth up --os linux
berth view          # open the printed 127.0.0.1 URL — guest only
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"berth_screenshot","arguments":{}}}' | berth mcp
berth end
```

Expected: a 1280×800-class PNG of openbox/Chromium inside the container; host Finder / host Cursor unchanged; after `berth end`, the view URL no longer accepts connections.

## What this is not

- A wallet, an x402 paywall, or a listing catalog
- A way to watch or drive the operator's logged-in desktop
- A bind-all noVNC on the LAN
