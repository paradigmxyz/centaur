#!/usr/bin/env bash
set -euo pipefail

home="${HOME:-/home/agent}"
state="$home/.centaur"
env_file="$state/env.sh"
mkdir -p "$state" "$home/workspace"

# The brain writes the lease's placeholder environment here. Keep an existing
# file: a derived image or a previous lease may already have populated it.
if [ ! -e "$env_file" ]; then
    (umask 077 && : > "$env_file")
fi
export BASH_ENV="$env_file"

# A hand runs as uid 1001, so it cannot update the system trust store. Compose a
# writable bundle from the image's roots plus the certificate-only mount and
# point every client that honors one of these variables at it.
ca_mount="/etc/centaur-hand/ca.crt"
if [ -s "$ca_mount" ]; then
    # Always start from the image roots: hands may egress directly, not only
    # through the proxy, so public CAs must stay trusted.
    system_bundle="/etc/ssl/certs/ca-certificates.crt"
    combined="$state/ca-certificates.crt"
    if [ -r "$system_bundle" ]; then
        cat "$system_bundle" "$ca_mount" > "$combined"
    else
        cat "$ca_mount" > "$combined"
    fi
    export SSL_CERT_FILE="$combined"
    export REQUESTS_CA_BUNDLE="$combined"
    export NODE_EXTRA_CA_CERTS="$combined"
    export GIT_SSL_CAINFO="$combined"
    export CURL_CA_BUNDLE="$combined"
fi

exec /usr/local/bin/centaur-hand "$@"
