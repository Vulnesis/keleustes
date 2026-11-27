# Keleustes tasks. `just` lists them.

belai := "../belai"

default:
    @just --list

# fmt check, clippy, tests (SQLite; set KLS_TEST_POSTGRES_URL to add Postgres)
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

build:
    cargo build --release

# open the desktop app
run *ARGS:
    cargo run -p kls-app -- {{ARGS}}

# serve nodes without a window
headless *ARGS:
    cargo run -p kls-app -- --headless {{ARGS}}

# regenerate belai's Go stubs from proto/ (needs protoc-gen-go, protoc-gen-go-grpc)
proto-go:
    protoc -I proto --go_out={{belai}} --go_opt=module=github.com/vulnetix/belai \
      --go-grpc_out={{belai}} --go-grpc_opt=module=github.com/vulnetix/belai \
      keleustes/node/v1/node.proto keleustes/node/v1/host.proto

# live forge suite: Gitea and Forgejo in podman or docker with a real tea (opt in;
# skipped when no container engine or tea; not part of `check`)
forge-live:
    bash crates/kls-github/live/forge-live.sh

# belai against a real headless Keleustes: join, link, board, revoke, then
# belai rc taking a dispatched session (transcript, prompt, stop)
e2e:
    cargo build -p kls-app
    cd {{belai}} && KELEUSTES_BIN={{justfile_directory()}}/target/debug/keleustes \
      go test -count=1 -run TestKeleustesEndToEnd -v ./internal/factorylink/
    cd {{belai}} && KELEUSTES_BIN={{justfile_directory()}}/target/debug/keleustes \
      go test -count=1 -run 'TestKeleustes(RemoteControl|CrossHost)$' -v ./e2e/

# installers for this platform into target/packages (needs: cargo install cargo-packager --locked)
# e.g. `just package`, `just package deb,appimage`
package formats="default":
    cargo packager -p kls-app --release --formats {{formats}}

# an RPM (needs: cargo install cargo-generate-rpm)
package-rpm:
    cargo build --release -p kls-app
    cargo generate-rpm -p crates/kls-app -o target/packages/

# register keleustes:// links and the menu entry for this user (Linux, from a dev build)
install-desktop:
    cargo build --release -p kls-app
    install -Dm755 target/release/keleustes ~/.local/bin/keleustes
    install -Dm644 packaging/keleustes.desktop ~/.local/share/applications/keleustes.desktop
    install -Dm644 packaging/icons/keleustes.svg ~/.local/share/icons/hicolor/scalable/apps/keleustes.svg
    for s in 32 64 128 256 512; do install -Dm644 packaging/icons/keleustes-$s.png ~/.local/share/icons/hicolor/${s}x${s}/apps/keleustes.png; done
    xdg-mime default keleustes.desktop x-scheme-handler/keleustes

# regenerate packaging/icons (PNG, ICO, SVG) from the brand mark (kls_ui::brand)
icons:
    cargo test -p kls-ui --lib brand::tests::write_packaging_icons -- --ignored --exact
