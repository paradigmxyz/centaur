#!/bin/bash
set -euo pipefail

case "${CENTAUR_GIT_COMMIT_SIGNING_ENABLED:-false}" in
    0|false|False|FALSE|no|No|NO|off|Off|OFF)
        exit 0
        ;;
    1|true|True|TRUE|yes|Yes|YES|on|On|ON)
        ;;
    *)
        echo "invalid CENTAUR_GIT_COMMIT_SIGNING_ENABLED value" >&2
        exit 1
        ;;
esac

key_path="${CENTAUR_GIT_COMMIT_SIGNING_KEY_PATH:-/var/run/secrets/centaur/git-signing/private-key.asc}"
if [ ! -r "$key_path" ]; then
    echo "git commit signing key is not readable: $key_path" >&2
    exit 1
fi

export GNUPGHOME="${GNUPGHOME:-${HOME:?}/.gnupg}"
install -d -m 0700 "$GNUPGHOME"

if ! key_listing="$(gpg --batch --with-colons --import-options show-only --import "$key_path" 2>/dev/null)"; then
    echo "git commit signing key is not valid OpenPGP key material" >&2
    exit 1
fi
fingerprints=()
while IFS= read -r candidate; do
    fingerprints[${#fingerprints[@]}]="$candidate"
done < <(
    printf '%s\n' "$key_listing" \
        | awk -F: '$1 == "sec" { primary = 1; next } primary && $1 == "fpr" { print $10; primary = 0 }'
)
unset key_listing

if [ "${#fingerprints[@]}" -ne 1 ]; then
    echo "git commit signing Secret must contain exactly one OpenPGP secret key" >&2
    exit 1
fi
fingerprint="${fingerprints[0]}"

if ! gpg --batch --quiet --import "$key_path" >/dev/null 2>&1; then
    echo "failed to import git commit signing key" >&2
    exit 1
fi

payload="$(mktemp)"
signature="$(mktemp)"
trap 'rm -f "$payload" "$signature"' EXIT
printf 'centaur git signing preflight\n' > "$payload"
if ! gpg --batch --yes --pinentry-mode loopback --passphrase '' \
    --local-user "$fingerprint" --output "$signature" --detach-sign "$payload" \
    >/dev/null 2>&1; then
    echo "git commit signing key cannot sign non-interactively" >&2
    exit 1
fi

git config --global gpg.program gpg
git config --global user.signingKey "$fingerprint"
git config --global commit.gpgSign true
