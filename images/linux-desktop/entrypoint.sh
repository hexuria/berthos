#!/bin/sh
# Start an isolated graphical session. This process runs *inside* the guest.
# It must not inherit host DISPLAY, host credentials, or a default route.
set -eu

Xvfb "${DISPLAY}" -screen 0 1280x800x24 -nolisten tcp &
xvfb_pid=$!

# Give X a moment so openbox/chromium do not race an empty socket.
i=0
while [ "$i" -lt 50 ]; do
  if xdpyinfo >/dev/null 2>&1; then
    break
  fi
  i=$((i + 1))
  sleep 0.1
done

openbox &

# Container Chromium needs --no-sandbox; this is still inside the guest, not the host.
chromium \
  --no-first-run \
  --disable-gpu \
  --disable-dev-shm-usage \
  --no-sandbox \
  --user-data-dir=/home/agent/.chromium \
  about:blank &

wait "$xvfb_pid"
