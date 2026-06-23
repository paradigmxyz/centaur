#!/bin/sh
set -eu

CONFIG_DIR="/etc/iron-proxy"
CONFIG_FILE="$CONFIG_DIR/proxy.yaml"
DEFAULT_CONFIG="/usr/local/share/iron-proxy/proxy.yaml.default"
CA_CERT="$CONFIG_DIR/ca.crt"
CA_KEY="$CONFIG_DIR/ca.key"
CERT_SHARE="/certs"
CA_MOUNT_DIR="${IRON_PROXY_CA_MOUNT:-/etc/iron-proxy-ca}"

log_json() {
    printf '{"timestamp":"%s","level":"%s","service":"iron-proxy","event":"%s","msg":"%s"}\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1" "$2" "$3"
}

mkdir -p "$CONFIG_DIR" "$CERT_SHARE"

# ── Load required CA from the pre-created Kubernetes Secret volume ─────────
if [ ! -f "$CA_MOUNT_DIR/ca-cert.pem" ] || [ ! -f "$CA_MOUNT_DIR/ca-key.pem" ]; then
    log_json "error" "ca_unavailable" "required CA files not found at $CA_MOUNT_DIR; refusing to start"
    exit 1
fi

cp "$CA_MOUNT_DIR/ca-cert.pem" "$CA_CERT"
cp "$CA_MOUNT_DIR/ca-key.pem" "$CA_KEY"
chmod 600 "$CA_KEY"
log_json "info" "ca_loaded" "loaded CA from mounted volume at $CA_MOUNT_DIR"

# ── Share CA cert with sandboxes ──────────────────────────────────────────
cp "$CA_CERT" "$CERT_SHARE/ca-cert.pem"
log_json "info" "ca_shared" "CA cert shared at /certs/ca-cert.pem"

# ── Managed mode: the control plane is the source of truth for the proxy
# config, and everything local comes from IRON_* env vars, so run with no
# -config. A local config file would conflict (e.g. management.listen). ────
if [ -n "${IRON_CONTROL_PLANE_URL:-}" ]; then
    log_json "info" "managed_mode" "IRON_CONTROL_PLANE_URL set; running without a local config"
    exec iron-proxy
fi

# ── Unmanaged mode: seed the local config from the baked default and pass it
# explicitly. `set -C` + `>` opens with O_EXCL — atomic create-only, no TOCTOU
# race with a concurrent writer (cp -n / mv -n are check-then-write). ───────
if (set -C; cat "$DEFAULT_CONFIG" > "$CONFIG_FILE") 2>/dev/null; then
    log_json "info" "config_seeded" "seeded $CONFIG_FILE from default"
fi

exec iron-proxy -config "$CONFIG_FILE"
