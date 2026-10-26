#!/bin/sh
# certbot deploy hook: give the gateway the new certificate and restart it.
set -eu
install -d -m 0750 -o root -g gateway /etc/crossfir3d/tls
install -m 0640 -o root -g gateway "$RENEWED_LINEAGE/fullchain.pem" "$RENEWED_LINEAGE/privkey.pem" /etc/crossfir3d/tls/
systemctl try-restart gateway
