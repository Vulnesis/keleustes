# Keleustes

The desktop Software Factory. A native Rust app (egui) that is the server
for a fleet of [belai](../belai) hosts: no web console, no sign-in, and no
sandboxes. The name is the trireme officer who called the stroke for every
rower.

## Install

| Method | Platforms | Command |
|---|---|---|
| Shell installer | Linux, macOS | `curl -fsSL https://raw.githubusercontent.com/Vulnesis/keleustes/main/install.sh \| sh` |
| Homebrew | macOS, Linux | `brew install vulnetix/tap/keleustes` |
| Scoop | Windows | `scoop bucket add vulnetix https://github.com/Vulnesis/scoop-bucket; scoop install keleustes` |
| Nix | Linux, macOS | `nix profile install github:Vulnesis/keleustes` |
| NixOS | Linux | `services.keleustes.enable = true;` (headless; see the docs) |
| Binary download | All | [GitHub Releases](https://github.com/Vulnesis/keleustes/releases/latest) |

Builds: linux amd64/arm64, darwin amd64/arm64, windows amd64. The
installer checks the Ed25519 signature on the release's `checksums.txt` (once a
release key is pinned), the SHA-256 against it and,
with `cosign` on `PATH`, the signed SLSA provenance. On Linux the window
needs GTK 3 and libayatana-appindicator (tray). Details, options and
uninstalling: [docs/install.md](docs/install.md).

## Quick start

```sh
just run                       # open the app; it serves nodes on 0.0.0.0:7443
# Fleet → Add node → copy the /connect command, then on the host:
/connect kls1.…                # in the belai TUI, or: belai connect kls1.…
belai rc --dir ~/src/project   # the host now takes sessions from Keleustes
```

From a shell beside the running app (same data directory):

```sh
keleustes token new --label build-box          # single-use join token
keleustes nodes list
keleustes nodes revoke <node-id>
keleustes sessions dispatch --node build-box --cwd ~/src/project \
  --prompt "fix the flaky test" --wait 60        # prints the session id
keleustes sessions list --open
keleustes sessions transcript <session-id> --follow
keleustes sessions prompt <session-id> "now add a regression test"
keleustes sessions command <session-id> stop
keleustes sessions teleport <session-id> --to other-box   # then on that host: belai -teleport <id>
keleustes config node.listen_addr 0.0.0.0:7443
keleustes config node.advertise_addrs 10.0.0.5:7443   # what tokens tell nodes to dial
keleustes open keleustes://sessions/<session-id>      # focus the running window
keleustes --headless                                  # serve without a window
```

Commands work against the data directory beside a running app: the app
picks up their events and outbox frames within a couple of seconds.

Data lives in the platform data directory (`~/.local/share/keleustes` on
Linux), or `--data DIR`: `keleustes.db` (SQLite), `ca/` (local CA and
server key), `artifacts/` (teleport bundles and evidence). Use Postgres
with `--database-url postgres://…`. The vault master key is in the OS
keyring (a 0600 `vault.key` file when there is none).

## Layout

| Crate | Role |
|---|---|
| `kls-proto` | node protocol, generated from `proto/keleustes/node/v1/node.proto` |
| `kls-store` | SQLite/Postgres store over sqlx `Any`, per-dialect migrations |
| `kls-core` | domain: sessions, board, memory, A2A, needs, audit, trust, settings, event bus |
| `kls-node` | local CA, join tokens, gRPC services (Node, Sessions, Board, Jobs, Teleport), gateway |
| `kls-engine` | workflows: graph, validation, dry run, executor, cron, templates, approvals |
| `kls-forge` | GitHub through local `gh` or a node's `gh`: repos, pulls, issues, conflicts, delivery |
| `kls-vault` | provider keys sealed with XChaCha20-Poly1305; grants to nodes |
| `kls-ui` | egui screens, ThreatCanary theme (light/dark), Phosphor icons |
| `kls-app` | the `keleustes` binary: window, headless, CLI, deep links, notifications, packaging |

How the work was split, and the conventions every area follows:
[docs/WORKSTREAMS.md](docs/WORKSTREAMS.md).

## Node protocol

See [belai docs/keleustes.md](../belai/docs/keleustes.md) for the node side.
In short:

- A token is `kls1.` followed by base64url JSON. It holds the server id,
  the addresses to dial, the server SPKI pin and a single-use 256-bit secret.
- Join uses TLS pinned to that key. The node sends a CSR, and the local CA
  issues a client certificate. Everything after that is mutual TLS 1.3.
- Link is one control stream per host, held by one belai process. It
  carries presence, dispatches, jobs and factory control as sequenced,
  acked frames that are replayed from an outbox after a reconnect. Vault
  key values never reach the outbox table: they are held in memory and put
  back into a frame when it is sent.
- Sessions, Board, Jobs and Teleport are plain RPCs any belai process on the
  host calls with the node certificate.

## Status

| Milestone | Scope | Status |
|---|---|---|
| M0 | workspace, protocol, store, CA and join, belai `/connect`, Fleet, Events | done |
| M1 | session sync, transcript, prompts, answers, commands, dispatch | done |
| M2 | board/kanban, memory, A2A and blackout, kill switch, Cockpit | done |
| M3 | workflows, templates, schedules, runs, approvals, checkpoints | done |
| M4 | GitHub through local `gh` or a node, Repos, Conflicts, signing, Delivery | done |
| M5 | vault, budgets, providers, model jobs on nodes, agent builder, crews, library | done |
| M6 | trust, audit chain, settings YAML, teleport, deep links, notifications, packaging | done |

Closing the window keeps Keleustes serving nodes, and the tray icon offers
Open and Quit. On Wayland an app cannot un-minimise itself, so Open (and a
`keleustes open` link) highlights the window in the taskbar; restore it from
there.

Not built yet:
- **Auto-update:** there is no release channel to update from yet.

## Checks

```sh
just check   # fmt, clippy -D warnings, tests (KLS_TEST_POSTGRES_URL adds Postgres)
just e2e     # belai joins a real headless Keleustes
just package # deb / pacman / AppImage via cargo-packager
```

The acceptance test lives in belai and drives both real binaries end to
end. It runs a headless Keleustes, mints a token, joins with `belai
connect`, and starts `belai rc`. It then dispatches a session, checks the
transcript, sends a prompt and runs a stop:

```sh
CARGO_TARGET_DIR=target/coord cargo build -p kls-app
cd ../belai && KELEUSTES_BIN=../keleustes/target/coord/debug/keleustes \
  go test -run TestKeleustesRemoteControl -v ./e2e/
```
