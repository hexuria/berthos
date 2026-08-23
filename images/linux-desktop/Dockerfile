# Isolated Linux desktop guest for Berthos v1.
#
# The doctor requires these labels. An image without them is refused.
# Default-deny egress is enforced by the node (`docker run --network none`).
# Do not bake secrets, SSH keys, cloud creds, or pairing tokens into this image.

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
        openbox \
        x11-xserver-utils \
        xvfb \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 1000 --shell /bin/bash agent

COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod 0755 /usr/local/bin/entrypoint.sh

USER agent
WORKDIR /home/agent

# No ports published by default. The node does not map host display sockets.
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
