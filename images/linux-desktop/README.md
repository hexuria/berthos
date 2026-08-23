# linux-desktop guest

Labeled isolated Linux desktop used by the eligibility doctor.

```sh
docker build -t berthos-linux-desktop:v1 images/linux-desktop
```

## Labels the doctor requires

| Label | Value |
| --- | --- |
| `berthos.guest.version` | `v1` |
| `berthos.desktop` | `xvfb-openbox-chromium` |
| `berthos.egress.policy` | `default-deny` |

Bump `berthos.guest.version` and the constant `REQUIRED_GUEST_VERSION` together when the contract changes. Old images fail closed.

## Default-deny egress

This image **does not** get a default route from the node.

- v1: `docker run --network none` (see `berthos-node` `DockerGuest`). No outbound, including DNS. Empty allowlist is deny-all, not "allow all".
- Later: a node-side allowlist proxy. Domains are allowlisted on the **node**, never by stuffing credentials or a wide-open resolver into the guest.

Do not add `--network host`, a default `iptables ACCEPT`, or a baked-in API key "for convenience." Secrets stay on the node.

## What is inside

Xvfb (`1280x800x24`) + openbox + Chromium, running as user `agent`. That is the leased desktop. It is not the operator's session.

Also inside the guest (still isolated):

- `x11vnc` on **guest** `127.0.0.1:5900` (the guest Xvfb only)
- noVNC / websockify on **guest** `127.0.0.1:6080` (unreachable from the host LAN; `--network none`)
- `/usr/local/bin/driver` → `action.sh` (screenshot / click / type / key via xdotool + ImageMagick `import` on `:99`)

The node maps that display through a loopback viewer and `docker exec`. It does not bind the host display.

## What must never be in the image

- Pairing tokens, `~/.berthos`, cloud keys, rclone configs
- Host cursor / host display sockets
- A pre-logged-in personal profile
