#!/bin/sh
# Installs or updates the game on the AWS server. Run as root (through SSM):
#   install.sh <bucket> <domain>
# Safe to run again: it installs the newest build and keeps the players' data.
set -eu
BUCKET=${1:?bucket}
DOMAIN=${2:?domain}
ARCH=$(uname -m)
HERE=/opt/crossfire/deploy

dnf install -y -q nftables dnf-automatic python3.11

# Security updates every day, applied.
sed -i 's/^upgrade_type *=.*/upgrade_type = security/; s/^apply_updates *=.*/apply_updates = yes/' /etc/dnf/automatic.conf
systemctl enable --now dnf-automatic.timer

for u in crossfire gateway; do
    id "$u" >/dev/null 2>&1 || useradd --system --no-create-home --shell /sbin/nologin "$u"
done

# The players' data volume (attached as the second disk), formatted the first time only.
DATA=$(lsblk -dpno NAME,SERIAL | awk '$2 ~ /^vol/ && $1 != "/dev/nvme0n1" {print $1; exit}')
[ -n "$DATA" ] || { echo "no data volume" >&2; exit 1; }
blkid "$DATA" >/dev/null || mkfs.xfs -q -L cf-data "$DATA"
mkdir -p /srv/crossfire
grep -q 'LABEL=cf-data' /etc/fstab || echo 'LABEL=cf-data /srv/crossfire xfs defaults,nodev,nosuid,noexec 0 2' >> /etc/fstab
mountpoint -q /srv/crossfire || mount /srv/crossfire

# The newest build, replacing the old one.
systemctl stop gateway crossfire 2>/dev/null || true
aws s3 cp --only-show-errors "s3://$BUCKET/crossfir3d-server-$ARCH.tar.gz" /tmp/build.tar.gz
rm -rf /opt/crossfire
# Owned by root, whatever the archive says: no other user may change the programs.
tar -xzf /tmp/build.tar.gz -C / --no-same-owner
chown -R root:root /opt/crossfire
rm /tmp/build.tar.gz
# No DMs from the start: the shipped dm_file holds only comments.
sed -i '/^[^#]/d' /opt/crossfire/etc/crossfire/dm_file
# This server's own welcome and news, not Crossfire's defaults.
install -m 0644 "$HERE/motd" "$HERE/news" /opt/crossfire/etc/crossfire/
[ -n "$(ls -A /srv/crossfire)" ] || cp -a /opt/crossfire/var-template/. /srv/crossfire/
chown -R crossfire:crossfire /srv/crossfire
chmod 0700 /srv/crossfire

# The C server's port: closed to everything but this machine.
install -m 0644 "$HERE/crossfir3d.nft" /etc/nftables/crossfir3d.nft
grep -q crossfir3d.nft /etc/sysconfig/nftables.conf || echo 'include "/etc/nftables/crossfir3d.nft"' >> /etc/sysconfig/nftables.conf
systemctl enable nftables
systemctl restart nftables

# The certificate: Let's Encrypt, proved through Route 53 (the instance role may only
# change this domain's records). No email is sent to Let's Encrypt.
if [ ! -x /opt/certbot/bin/python3.11 ]; then
    rm -rf /opt/certbot
    python3.11 -m venv /opt/certbot
    /opt/certbot/bin/pip install -q certbot certbot-dns-route53
fi
install -m 0755 "$HERE/renewed.sh" /etc/crossfir3d-renewed.sh
/opt/certbot/bin/certbot certonly -n --agree-tos --register-unsafely-without-email \
    --dns-route53 -d "$DOMAIN" --deploy-hook /etc/crossfir3d-renewed.sh --keep-until-expiring
RENEWED_LINEAGE=/etc/letsencrypt/live/$DOMAIN /etc/crossfir3d-renewed.sh
cat > /etc/systemd/system/certbot-renew.service <<'EOF'
[Unit]
Description=Renew the gateway's certificate
[Service]
Type=oneshot
ExecStart=/opt/certbot/bin/certbot renew -q
EOF
cat > /etc/systemd/system/certbot-renew.timer <<'EOF'
[Unit]
Description=Renew the gateway's certificate twice a day
[Timer]
OnCalendar=*-*-* 03,15:17
RandomizedDelaySec=1h
Persistent=true
[Install]
WantedBy=timers.target
EOF

for s in crossfire gateway; do
    install -m 0644 "$HERE/$s.service" /etc/systemd/system/
    install -d "/etc/systemd/system/$s.service.d"
    install -m 0644 "$HERE/sandbox.conf" "/etc/systemd/system/$s.service.d/sandbox.conf"
done
systemctl daemon-reload
systemctl enable --now certbot-renew.timer crossfire gateway
systemctl --no-pager --lines=0 status crossfire gateway
