#!/bin/sh
# Start an isolated graphical session. This process runs *inside* the guest.
# It must not inherit host DISPLAY, host credentials, or a default route.
set -eu

# Always the guest Xvfb. Ignore inbound DISPLAY so a host X11 cannot hijack.
export DISPLAY=:99

Xvfb "${DISPLAY}" -screen 0 1280x800x24 -nolisten tcp \
  +extension RANDR +extension XTEST \
  >/tmp/xvfb.log 2>&1 &
xvfb_pid=$!

# Give X a moment so openbox/chromium/x11vnc do not race an empty socket.
i=0
while [ "$i" -lt 50 ]; do
  if xdpyinfo >/dev/null 2>&1; then
    break
  fi
  i=$((i + 1))
  sleep 0.1
done

openbox >/tmp/openbox.log 2>&1 &

# Container Chromium needs --no-sandbox; this is still inside the guest, not the host.
chromium \
  --no-first-run \
  --disable-gpu \
  --disable-dev-shm-usage \
  --no-sandbox \
  --user-data-dir=/home/agent/.chromium \
  about:blank >/tmp/chromium.log 2>&1 &

# localhost VNC of the guest Xvfb. Host DISPLAY / host cursor are never used.
x11vnc -display :99 -forever -shared -nopw -localhost \
  -rfbport 5900 -wait 10 -noxdamage -repeat \
  >/tmp/x11vnc.log 2>&1 &

# noVNC on guest :6080. With --network none this is only reachable via the
# node's loopback proxy (docker exec socat), never as a LAN bind.
websockify --web=/usr/share/novnc 127.0.0.1:6080 127.0.0.1:5900 \
  >/tmp/novnc.log 2>&1 &

touch /tmp/berthos-ready

wait "$xvfb_pid"
