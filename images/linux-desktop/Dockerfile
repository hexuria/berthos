# Isolated Linux desktop guest for Berthos v1.
#
# The doctor requires these labels. An image without them is refused.
# Default-deny egress is enforced by the node (`docker run --network none`).
# Do not bake secrets, SSH keys, cloud creds, or pairing tokens into this image.
# The leased desktop is Xvfb + openbox + Chromium. It is not the host session.

FROM debian:bookworm-slim

LABEL org.opencontainers.image.title="berthos-linux-desktop" \
      org.opencontainers.image.description="Xvfb + openbox + Chromium guest. Not the host desktop." \
      berthos.guest.version="v1" \
      berthos.desktop="xvfb-openbox-chromium" \
      berthos.egress.policy="default-deny"

ENV DEBIAN_FRONTEND=noninteractive \
    DISPLAY=:99 \
    LANG=C.UTF-8

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        chromium \
        fonts-dejavu-core \
        imagemagick \
        novnc \
        openbox \
        socat \
        websockify \
        x11-utils \
        x11-xserver-utils \
        x11vnc \
        xdotool \
        xvfb \
    && rm -rf /var/lib/apt/lists/* \
    && ln -sf /usr/share/novnc/vnc.html /usr/share/novnc/index.html \
    && useradd --create-home --uid 1000 --shell /bin/bash agent

COPY entrypoint.sh /usr/local/bin/entrypoint.sh
COPY scripts/action.sh /usr/local/bin/action.sh
RUN chmod 0755 /usr/local/bin/entrypoint.sh /usr/local/bin/action.sh \
    && ln -sf action.sh /usr/local/bin/driver

USER agent
WORKDIR /home/agent

# noVNC listens on guest loopback only (network none). The node maps it
# through a host loopback proxy; it is never published on 0.0.0.0.
# The node does not map host display sockets.
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
