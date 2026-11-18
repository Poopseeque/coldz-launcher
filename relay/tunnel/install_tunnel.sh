#!/bin/sh
# COLDZ relay: the sign-in tunnel for the launcher (0.4.0) on a VPS that already runs coldz-relay-nginx
# (F:\COLDZ-work\relay_1611, "Зеркало CDN на релеях"). Run as root with the files of this folder next to it:
#     sh install_tunnel.sh
# What it does: a self-signed certificate (10 years, ECDSA P-256) in /etc/coldz/tunnel, the stream config next to
# nginx.conf, one `include` line inside its stream block, `nginx -t`, reload. Prints the SHA-256 for news.json.
# Backup of nginx.conf: /var/backups/coldz-relay-<date>-tunnel/. Nothing else on the VPS is touched.
# Undo: remove the include line from /etc/coldz/nginx/nginx.conf, `systemctl reload coldz-relay-nginx`.
set -eu
D=$(cd "$(dirname "$0")" && pwd)
CONF=/etc/coldz/nginx/nginx.conf
B=/var/backups/coldz-relay-$(date +%Y%m%d)-tunnel
test -f "$CONF" || { echo "no $CONF: this VPS has no coldz-relay-nginx"; exit 1; }
# the stream TLS module: Ubuntu's libnginx-mod-stream has it, check anyway
nginx -V 2>&1 | grep -q -- '--with-stream_ssl_module' || { echo "nginx without stream_ssl_module"; exit 1; }
nginx -V 2>&1 | grep -q -- '--with-stream_ssl_preread_module' || { echo "nginx without stream_ssl_preread_module"; exit 1; }

mkdir -p "$B"; cp -a "$CONF" "$B/nginx.conf"
install -d -m 750 -o root -g www-data /etc/coldz/tunnel
if [ ! -f /etc/coldz/tunnel/tunnel.key ]; then
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 3650 \
        -keyout /etc/coldz/tunnel/tunnel.key -out /etc/coldz/tunnel/tunnel.crt -subj "/CN=coldz-relay" 2>/dev/null
    chmod 640 /etc/coldz/tunnel/tunnel.key; chgrp www-data /etc/coldz/tunnel/tunnel.key
fi
install -m 644 "$D/tunnel.stream.conf" /etc/coldz/nginx/tunnel.stream.conf
if ! grep -q 'tunnel.stream.conf' "$CONF"; then
    # the include goes right after `stream {`
    sed -i 's|^stream {|stream {\n    include /etc/coldz/nginx/tunnel.stream.conf;|' "$CONF"
fi
nginx -t -c "$CONF"
systemctl reload coldz-relay-nginx.service || systemctl restart coldz-relay-nginx.service
# ufw active (Almaty): open the port
if ufw status 2>/dev/null | grep -q '^Status: active'; then ufw allow proto tcp to any port 8443 comment 'COLDZ launcher sign-in tunnel' >/dev/null; fi
ss -ltn | grep -q ':8443 ' && echo "listening on 8443" || { echo "8443 is NOT listening"; exit 1; }
echo "news.json signin.tunnels entry:"
FP=$(openssl x509 -in /etc/coldz/tunnel/tunnel.crt -noout -fingerprint -sha256 | sed 's/.*=//; s/://g' | tr 'A-F' 'a-f')
IP=$(curl -4 -s --max-time 5 https://ifconfig.me || hostname -I | awk '{print $1}')
printf '  { "address": "%s:8443", "sha256": "%s" }\n' "$IP" "$FP"
echo "backup: $B"
