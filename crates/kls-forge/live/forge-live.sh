#!/usr/bin/env bash
# Live forge suite (docs/gaps/forges.md 8.2): Gitea and Forgejo in a container
# (podman or docker), a real `tea`, the whole plane from discovery to a
# conflict commit pushed through `tea login helper`, then tea's OAuth sign-in
# approved headlessly (crates/kls-forge/tests/live_forge.rs). Opt in:
# `just forge-live`. Skips (exit 0) when no container engine or no tea, so it
# never blocks the default gate.
#
# tea: the host's `tea`, or `KLS_LIVE_TEA_RELEASE=<path>` to an official
# release binary (verify its sha256 against the release's checksums.txt
# first); the suite then runs in a small runner image holding that binary.
#
# Secrets (the account password and token) are random per run, kept in a
# mode-600 temp dir, handed over through the environment or files, never on
# a command line, and removed with the containers.
set -euo pipefail

engine=""
for e in podman docker; do
  if command -v "$e" >/dev/null 2>&1 && "$e" info >/dev/null 2>&1; then engine=$e; break; fi
done
if [[ -z "$engine" ]]; then
  echo "forge-live: no podman or docker; skipped"
  exit 0
fi
tea_release="${KLS_LIVE_TEA_RELEASE:-}"
if [[ -z "$tea_release" ]] && ! command -v tea >/dev/null 2>&1; then
  echo "forge-live: tea is not installed (or set KLS_LIVE_TEA_RELEASE); skipped"
  exit 0
fi

root="$(cd "$(dirname "$0")/../../.." && pwd)"
read -r -a images <<<"${KLS_LIVE_IMAGES:-codeberg.org/forgejo/forgejo:13 docker.io/gitea/gitea:1.22}"
port="${KLS_LIVE_PORT:-3300}"
user=e2e
fail=0
real_home="$HOME"
umask 077
secrets="$(mktemp -d)"
names=()
cleanup() {
  for n in "${names[@]}"; do "$engine" rm -f "$n" >/dev/null 2>&1 || true; done
  rm -rf "$secrets"
}
trap cleanup EXIT

# The test binary, built on the host once.
bin=$(cd "$root" && cargo test -p kls-forge --test live_forge --no-run --message-format=json 2>/dev/null \
  | jq -r 'select(.reason == "compiler-artifact" and .target.name == "live_forge" and .executable != null) | .executable' \
  | tail -n1)

runner=""
if [[ -n "$tea_release" ]]; then
  runner=localhost/kls-live-runner
  ctx="$secrets/ctx"; mkdir -p "$ctx"
  cp "$tea_release" "$ctx/tea"
  printf '%s\n' 'FROM docker.io/library/debian:trixie-slim' \
    'RUN export DEBIAN_FRONTEND=noninteractive && apt-get update && apt-get install -y --no-install-recommends git ca-certificates && rm -rf /var/lib/apt/lists/*' \
    'COPY --chmod=0755 tea /usr/local/bin/tea' >"$ctx/Containerfile"
  "$engine" build -q -t "$runner" -f "$ctx/Containerfile" "$ctx" >/dev/null
fi

for image in "${images[@]}"; do
  name="kls-live-$$-$port"
  kind=gitea
  binname=gitea
  if [[ "$image" == *forgejo* ]]; then kind=forgejo; binname=forgejo; fi
  host="localhost:$port"
  echo "== $image ($kind) on $host"
  names+=("$name")
  "$engine" run -d --rm --name "$name" -p "127.0.0.1:$port:3000" \
    -e GITEA__security__INSTALL_LOCK=true \
    -e GITEA__server__ROOT_URL="http://$host/" \
    -e GITEA__database__DB_TYPE=sqlite3 \
    -e FORGEJO__security__INSTALL_LOCK=true \
    -e FORGEJO__server__ROOT_URL="http://$host/" \
    -e FORGEJO__database__DB_TYPE=sqlite3 \
    "$image" >/dev/null
  for _ in $(seq 1 60); do
    curl -fsS "http://$host/api/v1/version" >/dev/null 2>&1 && break
    sleep 2
  done
  echo "   server: $(curl -fsS "http://$host/api/v1/version")"
  pass="$(head -c 24 /dev/urandom | base64 | tr -d '/+=' | head -c 24)Aa1!"
  E2E_PASS="$pass" "$engine" exec -u git -e E2E_PASS "$name" sh -c \
    "$binname admin user create --admin --username $user --password \"\$E2E_PASS\" --email $user@example.test --must-change-password=false" >/dev/null
  token=$("$engine" exec -u git "$name" "$binname" admin user generate-access-token \
    --username "$user" --token-name live --scopes all --raw | tail -n1)
  printf 'Authorization: token %s\n' "$token" >"$secrets/hdr"
  printf 'GITEA_SERVER_TOKEN=%s\nKLS_LIVE_FORGE_PASSWORD=%s\n' "$token" "$pass" >"$secrets/env"
  base="http://$host/api/v1"
  api() { curl -fsS -H @"$secrets/hdr" -H 'Content-Type: application/json' "$@"; }
  b64() { printf '%s' "$1" | base64 -w0; }
  sha_of() { api "$base/repos/$user/app/contents/app.txt?ref=$1" | jq -r .sha; }
  # <user>/app: main and feature conflict on app.txt; a pull request between them.
  api -X POST "$base/user/repos" -d '{"name":"app","auto_init":true,"default_branch":"main"}' >/dev/null
  api -X POST "$base/repos/$user/app/contents/app.txt" \
    -d "{\"content\":\"$(b64 $'a\n')\",\"message\":\"app\",\"branch\":\"main\"}" >/dev/null
  api -X POST "$base/repos/$user/app/branches" -d '{"new_branch_name":"feature","old_branch_name":"main"}' >/dev/null
  api -X PUT "$base/repos/$user/app/contents/app.txt" \
    -d "{\"content\":\"$(b64 $'feature\n')\",\"message\":\"feature\",\"branch\":\"feature\",\"sha\":\"$(sha_of feature)\"}" >/dev/null
  api -X PUT "$base/repos/$user/app/contents/app.txt" \
    -d "{\"content\":\"$(b64 $'main\n')\",\"message\":\"main\",\"branch\":\"main\",\"sha\":\"$(sha_of main)\"}" >/dev/null
  api -X POST "$base/repos/$user/app/pulls" -d '{"title":"Feature","head":"feature","base":"main"}' >/dev/null
  unset token

  # tea and git in a throwaway home, so the person's own logins are untouched.
  # tea reads the token from GITEA_SERVER_TOKEN (its `login add` help lists it).
  inner=$(cat <<EOF
set -e
export HOME="\$KLS_HOME" XDG_CONFIG_HOME="\$KLS_HOME/.config"
mkdir -p "\$XDG_CONFIG_HOME"
tea login add --name "$host" --url "http://$host" >/dev/null
unset GITEA_SERVER_TOKEN
git config --global user.name $user
git config --global user.email $user@example.test
KLS_LIVE_FORGE_HOST="$host" KLS_LIVE_FORGE_KIND="$kind" KLS_LIVE_FORGE_USER="$user" \
  KELEUSTES_FORGE_HTTP_HOSTS="$host" "\$KLS_BIN" --nocapture --test-threads 1
EOF
)
  ok=0
  if [[ -n "$runner" ]]; then
    "$engine" run --rm --network host --env-file "$secrets/env" -e KLS_HOME=/tmp/h -e KLS_BIN=/t/live_forge \
      -v "$bin:/t/live_forge:ro" "$runner" sh -c "$inner" && ok=1
  else
    tmp_home=$(mktemp -d)
    (set -a; . "$secrets/env"; set +a; cd "$root" && KLS_HOME="$tmp_home" KLS_BIN="$bin" \
      CARGO_HOME="${CARGO_HOME:-$real_home/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$real_home/.rustup}" \
      sh -c "$inner") && ok=1
    rm -rf "$tmp_home"
  fi
  if [[ $ok == 1 ]]; then echo "== $image: ok"; else echo "== $image: FAILED"; fail=1; fi
  rm -f "$secrets/hdr" "$secrets/env"
  unset pass
  "$engine" rm -f "$name" >/dev/null 2>&1 || true
  port=$((port + 1))
done
exit $fail
