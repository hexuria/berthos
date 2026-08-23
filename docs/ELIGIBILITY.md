# Eligibility doctor

A Berthos node **fails closed**. The doctor is a gate, not a dashboard. If a required check cannot be proven, the node is ineligible: `berth node up` will not listen, `POST /v1/park` and `POST /v1/leases` return `403`.

Warnings (optional tunnel) never change eligibility.

```
berth doctor                 # private intent
berth doctor --intent public
berth doctor --json
```

Live probes feed [`Facts`](../crates/berthos-protocol/src/eligibility.rs). Evaluation lives in [`berthos_node::evaluate`](../crates/berthos-node/src/eligibility.rs). Probe errors become conservative facts (`runtime_running=false`, `guest_image=None`, `free_vcpu=0`) — they are not omitted.

When Docker is available, `berth doctor` (no `--simulate`) talks to the daemon (`docker info`) and inspects `berthos-linux-desktop:v1` for the labeled contract. `--simulate` stays fail-closed only; it cannot produce `ok: true`.

## Required checks

| Id | Pass | Fail closed when |
| --- | --- | --- |
| `class` | `vm-guest` or `dedicated-server` | `class=laptop`. The host desktop is not a berth. Only a VM guest or a dedicated server guest may be leased. |
| `bind` | Listener is loopback (`127.0.0.1` / `::1`) | Bind-all (`0.0.0.0`, `::`, any non-loopback). Remote access is a later optional tunnel, not a wide-open socket. |
| `runtime` | Docker (or equivalent) answers `docker info` | Daemon down, binary missing, or probe error. |
| `guest_image` | Image `berthos-linux-desktop:v1` exists **and** carries the v1 labels | Image missing, inspect failed, or labels stale / absent. Rebuild; do not trust an unlabeled desktop. |
| `egress` | Policy is `default-deny` | `default-allow`. A desktop with a browser is a fraud appliance. Empty allowlist = no outbound, DNS included. v1 enforces this with `docker run --network none`. |
| `capacity` | At least **2** free vCPU and **4** GiB free RAM; neither may be `0` | Undersized, or the memory/CPU probe failed (`0` is not "unlimited"). |
| `guest_os` | `linux` | Windows Home/Pro OEM as a public listing; any Windows or macOS in v1; public macOS (out of scope). |
| `chassis` | Public: attested `server` or `vm-host`. Private: laptop chassis allowed **only** as the machine that *hosts* an isolated guest. | Public + `laptop` or `unknown`. Never accept a personal laptop or daily-driver as a public node. |
| `availability` | Public: operator attests `wired=true` and `always_on=true` | Public + lid-close / Wi-Fi-only / not always-on. Private loopback skips this (still isolated). |

## Optional check (warning only)

| Id | Meaning |
| --- | --- |
| `tunnel` | `cloudflared` (or equivalent) present. Missing is fine for loopback. A missing tunnel is **not** a reason to bind `0.0.0.0`. |

## Attestation schema (`GET /v1/eligibility`)

This is the stable JSON [berth-market](https://github.com/hexuria/berth-market) stores. It is the same document `berth doctor --json` prints. The market does not re-run isolation; it persists `ok`, `class`, `checks`, image labels, and `timestamp`.

```json
{
  "protocol": "v1",
  "source": "berthos.doctor",
  "ok": true,
  "eligible": true,
  "class": "vm-guest",
  "intent": "private",
  "checks": [
    { "id": "class", "status": "pass", "detail": "class=vm-guest (isolated guest, not the host desktop)" },
    { "id": "runtime", "status": "pass", "detail": "Docker (or equivalent) is running" },
    { "id": "guest_image", "status": "pass", "detail": "berthos-linux-desktop:v1 labels ok (v1, xvfb-openbox-chromium, default-deny)" }
  ],
  "image": {
    "name": "berthos-linux-desktop:v1",
    "labels": {
      "berthos.guest.version": "v1",
      "berthos.desktop": "xvfb-openbox-chromium",
      "berthos.egress.policy": "default-deny"
    }
  },
  "timestamp": "2026-08-23T07:21:00Z"
}
```

| Field | Type | Store? | Notes |
| --- | --- | --- | --- |
| `protocol` | string | yes | Wire version (`v1`). |
| `source` | string | yes | Always `berthos.doctor`. |
| `ok` | bool | **required** | `true` only when no required check failed. berth-market rejects `ok: false`. |
| `eligible` | bool | yes | Same value as `ok` (doctor wording). |
| `class` | string | **required** | `vm-guest` \| `dedicated-server` \| `laptop`. `laptop` is never eligible. There is no `host-desktop` class. |
| `intent` | string | yes | `private` \| `public`. |
| `checks` | array | **required** | Rows with `id`, `status` (`pass` \| `fail` \| `warn`), `detail`. |
| `image` | object \| null | **required** | `null` when the guest image is missing. Otherwise name + Docker labels. |
| `image.labels` | object | yes | Exact keys: `berthos.guest.version`, `berthos.desktop`, `berthos.egress.policy`. |
| `timestamp` | string | **required** | RFC 3339 UTC. When this report was evaluated. |

`ok` is the gate. A document with `ok: true` and `class: "laptop"` is inconsistent and must not be produced; evaluation refuses laptop first.

A production node re-probes Docker on `GET /v1/eligibility` so the stored attestation matches the daemon and image that exist *now*.

## Guest image labels

The doctor does not ask "does some desktop image exist?". It asks for a **versioned contract**:

```
LABEL berthos.guest.version="v1"
LABEL berthos.desktop="xvfb-openbox-chromium"
LABEL berthos.egress.policy="default-deny"
```

`berthos.desktop` means Xvfb + a desktop (openbox) + Chromium or equivalent. An image built before those labels existed inspects cleanly and still fails the doctor.

Build:

```sh
docker build -t berthos-linux-desktop:v1 images/linux-desktop
```

## Public vs private intent

| | Private (default, loopback) | Public (participate) |
| --- | --- | --- |
| Isolated Linux guest | required | required |
| Docker + labeled image | required | required |
| Loopback bind | required | required |
| `class=laptop` | rejected | rejected |
| Laptop *chassis* hosting Docker | allowed (you still lease the guest) | rejected |
| Wired + always-on | not required | required |
| Unknown chassis | allowed | rejected |
| Windows OEM / macOS | rejected (v1 Linux only) | rejected |
| Tunnel | optional warning | optional warning |

"Participate" means this node may be started and may accept leases. **Listing** that node for strangers' money is berth-market's job and is not implemented here. Public intent still has to pass the doctor so a later advertisement cannot lie about a laptop.

## Fail-closed unit tests

`cargo test` includes, among others:

- `laptop_class_rejected`
- `missing_image_rejected`
- `bind_all_rejected`

CLI smoke (no Docker required):

```sh
berth doctor --simulate laptop          # exit 1
berth doctor --simulate missing-image   # exit 1
berth doctor --simulate bind-all        # exit 1
```

`--simulate` cannot produce `eligible: true` / `ok: true`. There is no "pretend I passed" switch.

Live Docker tests **skip** when the daemon is missing (the default `linux` CI job). They run when Docker is present and the labeled image exists.

## CI and the live doctor

| Path | Docker | What runs |
| --- | --- | --- |
| `.github/workflows/ci.yml` job `linux` | may be present, image not built | `cargo test` (live probes skip or fail-closed on missing image) + `--simulate` smoke |
| `.github/workflows/ci.yml` job `docker-live` | required | `docker build -t berthos-linux-desktop:v1 images/linux-desktop`, then `berth doctor --json` (must be `ok: true`) and `cargo test` including isolated lease start/destroy |
| Manual box | required for a green doctor | same as Quick start |

If a runner has no Docker, keep the `linux` job; do not skip unit tests. The `docker-live` job is the documented path that builds the image and runs the live doctor.

## Manual path

On a machine with Docker:

1. `docker build -t berthos-linux-desktop:v1 images/linux-desktop`
2. Write `~/.berthos/node.toml` with `class = "vm-guest"`, `bind = "127.0.0.1"`.
3. `berth doctor` — every required row `pass`, `tunnel` may `warn`. JSON has `ok: true` and the image labels.
4. `berth node up` — prints a pairing code, listens on `127.0.0.1:7432`.
5. `GET http://127.0.0.1:7432/v1/eligibility` matches the CLI report (same schema).
6. `berth pair` then `berth up --os linux` — starts `docker run --network none`. `berth view` prints a loopback guest URL. `berth mcp` screenshots the guest. `DELETE /v1/leases/{id}` / `berth end` destroys the container, drops the view, and returns occupancy seconds.

If step 3 is red, stop. Do not "just start the node anyway."
