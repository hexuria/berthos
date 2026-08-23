# Architecture

Berthos v1 is a **computer-session node**. An agent (or the operator CLI) pairs with the node, the node holds exactly one isolated Linux desktop guest at a time, and occupancy is metered in seconds. Isolation is the product: the host desktop and host cursor are not on the trust boundary of a lease.

Payments, listings, and wallets are **out of this process**. A quote is a number of occupancy seconds plus a notional USD figure labeled `charged_here: false`. Settlement, if any, is the sibling repo [berth-market](https://github.com/hexuria/berth-market).

## Components

```
crates/berthos-cli        `berth` binary (doctor, node up, pair, up)
        │
        ▼
crates/berthos-node       library: eligibility gate + Axum on 127.0.0.1
        │
        ├── crates/berthos-protocol   lease / quote / receipt / doctor types
        └── images/linux-desktop      labeled guest (Xvfb + openbox + Chromium)
```

| Piece | Responsibility | Not responsible for |
| --- | --- | --- |
| `berthos-protocol` | Shared vocabulary. Occupancy unit is **seconds**. | Gas tokens, catalogs, x402 |
| `berthos-node` | Fail-closed doctor, pairing booth, park/unpark, lease create/end, guest destroy | Host desktop control, bind-all HTTP |
| `berthos-cli` | Operator/agent entrypoints | A console SPA, MCP, marketplace |
| `linux-desktop` | Reproducible isolated guest with versioned labels | Host networking, secrets |

The crates publish conceptually as **berthos**; the command they install is **`berth`**.

## Trust boundaries

```
┌─────────────────────────────────────────────────────────────┐
│ operator host (laptop, NUC, VM host, dedicated server)      │
│                                                             │
│  ~/.berthos/          pairing hashes, node.toml, client.toml│
│  berthos-node         loopback HTTP, doctor, Docker client  │
│           │                                                 │
│           │  docker run --network none                      │
│           │  no env secrets, no ~/.berthos mount            │
│           ▼                                                 │
│  ┌───────────────────────────────────────────────────────┐  │
│  │ guest  (Linux desktop)                                 │  │
│  │ untrusted tenant / agent                               │  │
│  │ cannot see host X11, host cursor, host files, tokens   │  │
│  └───────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
         ▲
         │ optional later tunnel (cloudflared). never bind-all.
     paired client (same host in v1 loopback)
```

**Inside the guest:** the session. Treat it as hostile. It gets a display and a browser, not the operator's cookies.

**On the node:** pairing tokens (hashed), doctor facts, Docker. This is the trusted computing base for isolation. A compromised guest must not become a compromised host.

**Outside this repo:** berth-market. The node may later *advertise* eligibility to that control plane. v1 does not implement the advertisement, the matcher, or money.

### What is never crossed

- Host `DISPLAY` / `/tmp/.X11-unix`
- `--network=host`
- Bind on `0.0.0.0` / `::`
- Mounting `~/.berthos` or any token file into the guest
- Passing cloud keys, rclone configs, or wallet seeds as lease fields
- Treating `X-Forwarded-For` as loopback

## Eligibility as a gate

Nothing that participates (node start, park, lease create) proceeds on a red doctor. Evaluation is pure and fail-closed: see [ELIGIBILITY.md](ELIGIBILITY.md).

`GET /v1/eligibility` returns a **storeable attestation**: `ok`, `class`, `checks[]`, image labels, and `timestamp` (`source` is `berthos.doctor`). berth-market persists that JSON; it does not re-run isolation. A production node re-probes Docker on that GET so the document matches the daemon and labels that exist now.

```
            observe facts
                 │
                 ▼
           evaluate(facts)          ← missing probe = fail, not skip
                 │
        ┌────────┴────────┐
        │                 │
   Ineligible        Eligible
   (fail closed)          │
                    ┌─────┴──────┐
                    │            │
                 Unparked     Parked  ← default after a green start
                    │            │
                    │            ▼
                    │         Leased (one live guest)
                    │            │
                    │            ▼  DELETE / end
                    │         Parked
                    │         guest destroyed (v1 revert)
                    └────────────┘
```

- **Ineligible → Parked** is impossible. `POST /v1/park` and `POST /v1/leases` return `403`.
- **`berth node up`** exits `1` if the doctor is red. The listener never comes up.
- **Unparked → Leased** is `409` (`node is unparked`).
- **Leased → Unparked** is `409` (`cannot unpark while a lease is live`).
- **Tunnel missing** is a warning. It cannot flip Ineligible to Eligible, and it cannot flip Eligible to Ineligible.

`class=laptop` is a terminal Ineligible, in every intent.

## Pairing

Capability tokens, not ambient trust.

1. Node start generates a one-time code `XXXX-XXXX`.
2. Loopback `GET /v1/pairing` reveals it. A non-loopback peer gets `404`. Forwarded headers are ignored.
3. `POST /v1/pair` `{ "code" }` returns a bearer token and rotates the code.
4. The node stores **SHA-256(token)** plus capabilities (`operator`, `lease`). The raw token is shown once and kept in `~/.berthos/client.toml` mode `0600` on the client.
5. Park/unpark require `operator`. Lease create/end require `lease`. v1 default pair grants both so one CLI can do both jobs.

A stolen guest cannot mint a token. The guest never sees the booth.

## Lease and occupancy

A lease is occupancy of an attested guest, not a click stream.

- Create: eligible + parked + paired + `os=linux` + `vcpu>0` + `mem_gib>0`. Then the runtime starts a guest.
- Meter: wall-clock seconds from `started_at` to end. Idle and busy cost the same, because holding the box is the scarce thing.
- Minimum: 60 seconds on the receipt (`billed_seconds = max(occupancy, min)`).
- Quote: default shape 2 vCPU / 4 GiB / 40 GiB, isolated, notional `$0.048/hr`. **Not charged.**
- End: destroy the guest, return a [`Receipt`](../crates/berthos-protocol/src/session.rs) whose `occupancy_unit` is `seconds`.

v1 is one live lease per node. A second create is `409`.

## Snapshot / revert

The intended production path is: snapshot (or mark a golden image) at lease start, revert at lease end, so the next tenant inherits nothing.

**v1 implements revert as destroy-and-recreate.** `DELETE /v1/leases/{id}` runs `docker rm -f`. The next `POST /v1/leases` starts a new container from `berthos-linux-desktop:v1`. That is weaker than a filesystem snapshot (slower cold start) and stronger than "leave the disk around" (no leftover profile). Do not persist `/home/agent` across tenants until a real snapshot path exists.

## Guest runtime contract

`DockerGuest` starts containers with:

- `--network none` — default-deny egress, including DNS
- `--memory` / `--cpus` from the quote
- labels `berthos.role=guest` and `berthos.lease=…`
- **no** `-e` secrets, **no** host bind-mounts of the node home, **no** host network, **no** host display

The image must already carry `berthos.guest.version=v1`, `berthos.desktop=xvfb-openbox-chromium`, and `berthos.egress.policy=default-deny`. The doctor inspects those labels. An image built before the contract existed inspects fine and is still refused.

After `docker run`, the node inspects the container and **refuses** it if `NetworkMode` is not `none`, if it is privileged, or if a host display socket (`/tmp/.X11-unix`, Wayland) was mounted. Host cursor / host `DISPLAY` are never passed in.

## What is deliberately absent

- MCP / computer-use adapters (next layer, not this skeleton)
- Operator console SPA
- Cloudflare tunnel spawn (doctor warns if `cloudflared` is missing; v1 loopback does not need it)
- Workspace volumes, S3, rclone
- Windows or macOS guests
- A listing catalog, a matcher, a wallet, a token

Those omissions are load-bearing. This repo stays a node.
