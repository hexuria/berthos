# Berthos

**A Berthos node is a parked computer-session that an agent can lease — never the host desktop.**

This repository is the **node**: a fail-closed eligibility doctor, a loopback-only HTTP daemon, a pairing booth, and an isolated Linux desktop guest image. It is the room where a session lives.

It is **not** the market. Listings, wallets, x402, USDC, and tokens live in the sibling repo **[berth-market](https://github.com/hexuria/berth-market)**. Quotes printed here are occupancy seconds. Nothing is charged in this process.

```
operator / agent CLI (berth)
        │  HTTP on 127.0.0.1 (tunnel optional, later)
        ▼
berthos-node  —  park / unpark / lease / eligibility
        │  isolated guest only
        ▼
Linux desktop (Xvfb + openbox + Chromium)  ← not the host cursor, not Finder
```

v1 is **Linux guest only**, loopback first. Public macOS is out of scope. Windows Home/Pro OEM on the metal is not a public listing. A private Windows VM for the operator's own agent is later, not this tree.

## Hard rules

These are product rules, not style nits. The doctor **fails closed**.

1. **Isolation is the product.** The node never rents or drives the host desktop / host cursor. No `--network=host`, no `/tmp/.X11-unix`, no host `DISPLAY`.
2. **`class=laptop` is rejected.** A personal laptop or daily-driver is never a public node.
3. **Only a VM guest or a dedicated server guest may be leased.**
4. **Eligibility before participation.** Docker (or equivalent) running, the labeled Linux desktop guest image present, default-deny egress, wired/always-on advertised for public intent, enough free vCPU/RAM, loopback bind only. Tunnel is optional. Missing probes fail, they are not skipped.
5. **Windows Home/Pro OEM on the metal is not a public listing.**
6. **Public macOS is out of scope for v1.**
7. **Secrets stay on the node.** Pairing tokens and operator files are never mounted into the guest.
8. **No payments here.** Do not look for a wallet, a token, or a listing catalog in this repo.

See [docs/ELIGIBILITY.md](docs/ELIGIBILITY.md) for the check list and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for trust boundaries and the state machine.

## How to run

Requires a stable Rust toolchain and, for a real guest, Docker.

```sh
cargo install --path crates/berthos-cli
# the crates are named berthos-*; the command they install is `berth`
```

### 1. Build the labeled guest image

The doctor will not pass without this image and its versioned labels.

```sh
docker build -t berthos-linux-desktop:v1 images/linux-desktop
```

Required labels (stamped by the Dockerfile):

| Label | Value |
| --- | --- |
| `berthos.guest.version` | `v1` |
| `berthos.desktop` | `xvfb-openbox-chromium` |
| `berthos.egress.policy` | `default-deny` |

An unlabeled or stale image is refused, not trusted. Rebuild after changing the contract. The node starts guests with `--network none` (empty allowlist = no outbound, DNS included). See [images/linux-desktop](images/linux-desktop).

### 2. Doctor

```sh
berth doctor                 # private loopback intent (default)
berth doctor --intent public # extra wired / always-on / chassis checks
berth doctor --json
```

Exit `0` only when every required check passed. Exit `1` otherwise.

Advertise the node in `~/.berthos/node.toml` (created on first `berth node up`):

```toml
class = "vm-guest"          # vm-guest | dedicated-server | laptop (laptop always fails)
chassis = "vm-host"         # server | vm-host | laptop | unknown
intent = "private"          # private | public
guest_os = "linux"
bind = "127.0.0.1"
port = 7432
always_on = true
wired = true
```

`class=laptop` fails in every intent. A laptop **chassis** may host a private loopback node that still leases an isolated guest. That chassis cannot go public.

### 3. Node

```sh
berth node up
# pairing code: ABCD-EFGH
# listening on http://127.0.0.1:7432
```

The process **refuses to start** if the doctor is red, and **refuses to listen** on anything but loopback. `berth node up --bind 0.0.0.0` is bind-all and is rejected.

Parked is the default (new leases allowed). Unpark while a lease is live is `409`.

### 4. Pair

Capability token, not a cookie on a URL.

```sh
berth pair --code ABCD-EFGH
# token stored in ~/.berthos/client.toml (mode 0600)
```

`GET /v1/pairing` reveals the code on loopback only. `X-Forwarded-For` is ignored.

### 5. Lease a Linux guest (local loopback)

```sh
berth up --os linux
# lease l_…
# quote seconds (min 60s) — not charged
```

`--os windows` and `--os macos` are rejected. Ending the lease destroy-and-recreates the guest (v1 revert; snapshot/restore is documented, not implemented). Occupancy is wall-clock seconds the guest is held, not clicks.

## HTTP (127.0.0.1 only)

| Method | Path | Auth | Notes |
| --- | --- | --- | --- |
| `GET` | `/health` | no | liveness |
| `GET` | `/v1/eligibility` | no | last doctor report |
| `GET` | `/v1/node` | no | parked / eligible / live lease |
| `POST` | `/v1/park` | operator | fail closed if ineligible |
| `POST` | `/v1/unpark` | operator | `409` if a lease is live |
| `GET` | `/v1/pairing` | loopback | current pairing code |
| `POST` | `/v1/pair` | code | returns a bearer token |
| `POST` | `/v1/leases` | lease | create; `os=linux` only |
| `GET` | `/v1/leases` | lease | live leases |
| `DELETE` | `/v1/leases/{id}` | lease | end; returns an occupancy receipt |

Authorization: `Authorization: Bearer <token>`.

## Doctor smoke (no Docker)

The CLI can **simulate failures**, not success:

```sh
berth doctor --simulate laptop          # exit 1 — class rejected
berth doctor --simulate missing-image   # exit 1
berth doctor --simulate bind-all        # exit 1
```

That is the automated smoke path. The manual path on a real box is the Quick start above: build the image, `berth doctor`, `berth node up`, `berth pair`, `berth up --os linux`.

`cargo test` covers the same fail-closed cases as unit tests.

## What this repo does not do

- Marketplace listings or a catalog of other people's nodes
- Wallets, USDC, x402, tokens, cash-out
- Driving the host desktop or host Cursor
- Public Windows OEM or public macOS
- Binding `0.0.0.0` and calling it a product

Talk to **[berth-market](https://github.com/hexuria/berth-market)** when you want spend/earn. Talk to this repo when you want a computer session that cannot see the operator's logged-in desktop.

## License

MIT. See [LICENSE](LICENSE).
