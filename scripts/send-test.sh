#!/usr/bin/env bash
# Send an example Linear payload to a running linear-webhook.
#
#   scripts/send-test.sh URL FIXTURE [SECRET]
#
#   URL      e.g. http://127.0.0.1:9999/server1
#   FIXTURE  e.g. examples/issue-update.json
#   SECRET   Linear signing secret; omit when running --dangerously-ignore-secret
#
# webhookTimestamp is rewritten to the current time so the replay check passes.
set -euo pipefail

if [ $# -lt 2 ] || [ $# -gt 3 ]; then
    sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
fi
url=$1
fixture=$2
secret=${3:-}

now_ms=$(date +%s%3N)
body=$(sed -E "s/\"webhookTimestamp\": *[0-9]+/\"webhookTimestamp\": ${now_ms}/" "$fixture")
delivery=$(cat /proc/sys/kernel/random/uuid)

args=(-sS -w '%{http_code}\n' -X POST "$url"
    -H 'Content-Type: application/json; charset=utf-8'
    -H "Linear-Delivery: ${delivery}"
    --data-binary "$body")
if [ -n "$secret" ]; then
    sig=$(printf '%s' "$body" | openssl dgst -sha256 -hmac "$secret" -hex | sed 's/^.*= //')
    args+=(-H "Linear-Signature: ${sig}")
fi
curl "${args[@]}"
