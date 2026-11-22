#!/usr/bin/env bash
# Live forge suite (docs/forges.md, design 8.2): Gitea and Forgejo in docker,
# a real `tea`, the whole plane from discovery to a conflict commit pushed
# through `tea login helper`. Opt in: `just forge-live`. Skips (exit 0) when
# docker or tea is missing, so it never blocks the default gate.
set -euo pipefail

if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
  echo "forge-live: docker is not available; skipped"
  exit 0
fi
if ! command -v tea >/dev/null 2>&1; then
  echo "forge-live: tea is not installed (sudo pacman -S tea); skipped"
  exit 0
fi

root="$(cd "$(dirname "$0")/../../.." && pwd)"
read -r -a images <<<"${KLS_LIVE_IMAGES:-gitea/gitea:1.22 codeberg.org/forgejo/forgejo:11}"
port=3300
pass='live-Pass-123!'
fail=0
real_home="$HOME"

for image in "${images[@]}"; do
  name="kls-live-$$-$port"
  kind=gitea
  bin=gitea
  if [[ "$image" == *forgejo* ]]; then kind=forgejo; bin=forgejo; fi
  echo "== $image ($kind) on 127.0.0.1:$port"
  docker run -d --rm --name "$name" -p "127.0.0.1:$port:3000" \
    -e GITEA__security__INSTALL_LOCK=true \
    -e GITEA__server__ROOT_URL="http://localhost:$port/" \
    -e GITEA__database__DB_TYPE=sqlite3 \
    -e FORGEJO__security__INSTALL_LOCK=true \
    -e FORGEJO__server__ROOT_URL="http://localhost:$port/" \
    -e FORGEJO__database__DB_TYPE=sqlite3 \
    "$image" >/dev/null
  trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT
  for _ in $(seq 1 60); do
    curl -fsS "http://localhost:$port/api/v1/version" >/dev/null 2>&1 && break
    sleep 2
  done
  docker exec -u git "$name" "$bin" admin user create --admin --username ana \
    --password "$pass" --email ana@example.com --must-change-password=false >/dev/null
  token=$(docker exec -u git "$name" "$bin" admin user generate-access-token \
    --username ana --token-name live --scopes all --raw | tail -n1)
  base="http://localhost:$port/api/v1"
  api() { curl -fsS -H "Authorization: token $token" -H 'Content-Type: application/json' "$@"; }
  b64() { printf '%s' "$1" | base64 -w0; }
  sha_of() { api "$base/repos/ana/app/contents/app.txt?ref=$1" | sed -n 's/.*"sha":"\([0-9a-f]*\)".*/\1/p' | head -n1; }
  # ana/app: main and feature conflict on app.txt; a pull request between them.
  api -X POST "$base/user/repos" -d '{"name":"app","auto_init":true,"default_branch":"main"}' >/dev/null
  api -X POST "$base/repos/ana/app/contents/app.txt" \
    -d "{\"content\":\"$(b64 $'a\n')\",\"message\":\"app\",\"branch\":\"main\"}" >/dev/null
  api -X POST "$base/repos/ana/app/branches" -d '{"new_branch_name":"feature","old_branch_name":"main"}' >/dev/null
  api -X PUT "$base/repos/ana/app/contents/app.txt" \
    -d "{\"content\":\"$(b64 $'feature\n')\",\"message\":\"feature\",\"branch\":\"feature\",\"sha\":\"$(sha_of feature)\"}" >/dev/null
  api -X PUT "$base/repos/ana/app/contents/app.txt" \
    -d "{\"content\":\"$(b64 $'main\n')\",\"message\":\"main\",\"branch\":\"main\",\"sha\":\"$(sha_of main)\"}" >/dev/null
  api -X POST "$base/repos/ana/app/pulls" -d '{"title":"Feature","head":"feature","base":"main"}' >/dev/null

  # tea and git in a throwaway home, so the person's own logins are untouched.
  tmp_home=$(mktemp -d)
  host="localhost:$port"
  HOME="$tmp_home" XDG_CONFIG_HOME="$tmp_home/.config" \
    tea login add --name "$host" --url "http://$host" --token "$token" >/dev/null
  HOME="$tmp_home" git config --global user.name ana
  HOME="$tmp_home" git config --global user.email ana@example.com

  if (cd "$root" && HOME="$tmp_home" XDG_CONFIG_HOME="$tmp_home/.config" \
        CARGO_HOME="${CARGO_HOME:-$real_home/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$real_home/.rustup}" \
        KLS_LIVE_FORGE_HOST="$host" KLS_LIVE_FORGE_KIND="$kind" KELEUSTES_FORGE_HTTP_HOSTS="$host" \
        cargo test -p kls-github --test live_forge --target-dir target/forge-live -- --nocapture); then
    echo "== $image: ok"
  else
    echo "== $image: FAILED"
    fail=1
  fi
  rm -rf "$tmp_home"
  docker rm -f "$name" >/dev/null 2>&1 || true
  trap - EXIT
  port=$((port + 1))
done
exit $fail
