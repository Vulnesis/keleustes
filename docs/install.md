# Install

Every release on GitHub carries one binary per platform, `install.sh`,
`checksums.txt`, `checksums.txt.sig` (once a release key is set) and
`provenance.sigstore.json`:

| Asset | Platform |
|---|---|
| `keleustes-linux-amd64` | Linux x86_64 (glibc 2.35 or newer) |
| `keleustes-linux-arm64` | Linux aarch64 (glibc 2.35 or newer) |
| `keleustes-darwin-arm64` | macOS 11 or newer, Apple silicon |
| `keleustes-darwin-amd64` | macOS 11 or newer, Intel |
| `keleustes-windows-amd64.exe` | Windows x86_64 (and arm64 under emulation) |

Homebrew, Scoop and the installer all install these same binaries.

## Shell installer (Linux, macOS)

```sh
curl -fsSL https://raw.githubusercontent.com/Vulnesis/keleustes/main/install.sh | sh
```

Options go after `sh -s --`:

```sh
curl -fsSL https://raw.githubusercontent.com/Vulnesis/keleustes/main/install.sh | sh -s -- --install-dir ~/.local/bin
curl -fsSL https://raw.githubusercontent.com/Vulnesis/keleustes/main/install.sh | sh -s -- --version v0.1.0
curl -fsSL https://raw.githubusercontent.com/Vulnesis/keleustes/main/install.sh | sh -s -- --require-provenance
```

| Option | Environment | Default |
|---|---|---|
| `--install-dir DIR` | `INSTALL_DIR` | `/usr/local/bin`, then `~/.local/bin` when that is not writable |
| `--version vX.Y.Z` | `VERSION` | latest (resolved to a tag once, so the binary and its checksum come from the same release) |
| `--require-provenance` | `KELEUSTES_REQUIRE_PROVENANCE=1` | off |
| `--require-signature` | `KELEUSTES_REQUIRE_SIGNATURE=1` | off |
| `--insecure-skip-verify` | `KELEUSTES_INSECURE_SKIP_VERIFY=1` | off |
| | `KELEUSTES_DOWNLOAD_BASE=URL` | fetch the assets from a mirror (or `file://` directory) |

What it checks, in order:

1. The download is complete (size against the server's `Content-Length`).
2. `checksums.txt` is signed by the release key. `install.sh` pins the
   Ed25519 public key (`RELEASE_PUBLIC_KEY`, the same value as the
   repository variable `VULNETIX_RELEASE_PUBLIC_KEY`), as the CLI embeds
   it in the sensor. With a key pinned, a missing or bad
   `checksums.txt.sig` stops the install. The check needs `openssl` with
   Ed25519 (OpenSSL 3 or newer; the LibreSSL in macOS cannot). Without
   one the check is skipped with a warning and the install goes on with
   steps 3 and 4. `--require-signature` refuses instead. With no key
   pinned, releases are unsigned and nothing is checked here
   (`--require-signature` then refuses).
3. Its SHA-256 matches `checksums.txt`. No SHA-256 tool, no
   `checksums.txt` or no entry for the asset stops the install, as a
   mismatch does.
4. With `cosign` on `PATH`, the release's SLSA v1 provenance names this
   exact binary, signed keyless by the `slsa-provenance.yml` workflow for
   this repository. A bundle that does not verify stops the install.

Then it runs `keleustes --version`, says on Linux whether GTK 3 and
libayatana-appindicator are present (and the command that installs them),
and says when the install directory is not on `PATH`.

The same checks by hand:

```sh
v=v0.1.0; a=keleustes-linux-amd64
base=https://github.com/Vulnesis/keleustes/releases/download/$v
curl -fsSLO $base/$a -O $base/checksums.txt -O $base/checksums.txt.sig -O $base/provenance.sigstore.json
pub=$(sed -n 's/^RELEASE_PUBLIC_KEY="\(.*\)"$/\1/p' install.sh)   # or the value from a trusted copy
{ printf '\060\052\060\005\006\003\053\145\160\003\041\000'; printf %s "$pub" | base64 -d; } > release-pub.der
base64 -d checksums.txt.sig > checksums.sig.raw
openssl pkeyutl -verify -pubin -keyform DER -inkey release-pub.der -rawin \
  -in checksums.txt -sigfile checksums.sig.raw
sha256sum --ignore-missing -c checksums.txt
cosign verify-blob-attestation --bundle provenance.sigstore.json --new-bundle-format \
  --type slsaprovenance1 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp '^https://github\.com/(Vulnetix|Vulnesis)/cli/\.github/workflows/slsa-provenance\.yml@' \
  --certificate-github-workflow-repository Vulnesis/keleustes \
  $a
```

## Linux runtime libraries

The binary links the system GTK 3 (the tray menu) and loads X11 or
Wayland, OpenGL and libayatana-appindicator at run time. `keleustes
--headless` needs GTK 3 installed but no display.

| Distribution | Command |
|---|---|
| Debian, Ubuntu | `sudo apt-get install libgtk-3-0 libayatana-appindicator3-1` |
| Fedora | `sudo dnf install gtk3 libayatana-appindicator-gtk3` |
| Arch | `sudo pacman -S gtk3 libayatana-appindicator` |
| openSUSE | `sudo zypper install gtk3 libayatana-appindicator3-1` |

Without appindicator the window runs without a tray icon. For the
application menu entry and `keleustes://` links, install
`packaging/keleustes.desktop` and the icon (`just install-desktop` from a
checkout does both); the Nix package installs them.

## Homebrew (macOS, Linux)

```sh
brew install vulnetix/tap/keleustes
brew upgrade vulnetix/tap/keleustes
```

A formula, not a cask, as for belai and the CLI: it installs the release
binary to `bin/keleustes`, launched from a terminal or the tray. On Linux
the formula's caveats list the GTK packages above.

## Scoop (Windows)

```powershell
scoop bucket add vulnetix https://github.com/Vulnesis/scoop-bucket
scoop install keleustes
scoop update keleustes
```

Scoop adds a Start menu shortcut, Keleustes.

## Nix

```sh
nix run github:Vulnesis/keleustes                  # try it
nix profile install github:Vulnesis/keleustes      # install
nix develop github:Vulnesis/keleustes              # toolchain, protoc, GTK for `cargo build`
```

The flake has `packages.<system>.keleustes` (also `default`),
`overlays.default` (adds `pkgs.keleustes`) and `nixosModules.default`.
The package builds from source with the workspace version in
`Cargo.toml`, and installs the desktop entry and icons on Linux.

### NixOS: headless service

```nix
{
  inputs.keleustes.url = "github:Vulnesis/keleustes";

  outputs = { nixpkgs, keleustes, ... }: {
    nixosConfigurations.factory = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        keleustes.nixosModules.default
        {
          services.keleustes = {
            enable = true;
            listenAddress = "0.0.0.0:7443";
            advertiseAddrs = [ "factory.example.internal:7443" ];
            openFirewall = true;
          };
        }
      ];
    };
  };
}
```

| Option | Default | |
|---|---|---|
| `enable` | `false` | run `keleustes --headless` as `keleustes.service` |
| `package` | this flake's package | |
| `user`, `group` | `keleustes` | created when left at the default |
| `dataDir` | `/var/lib/keleustes` | database, CA, artifacts, `vault.key` |
| `listenAddress` | `0.0.0.0:7443` | `node.listen_addr` |
| `advertiseAddrs` | `[]` | `node.advertise_addrs`, what join tokens tell nodes to dial |
| `databaseUrl` | `null` | Postgres instead of SQLite; put a password in `environmentFile` as `KELEUSTES_DATABASE_URL` |
| `environmentFile` | `null` | systemd `EnvironmentFile` |
| `settings` | `{}` | more `keleustes config KEY VALUE` settings |
| `openFirewall` | `false` | open the listen port |
| `extraArgs` | `[]` | appended to `keleustes --headless` |

Settings are written with `keleustes config` before each start, so the
module stays the source of truth. Mint a join token on the host:

```sh
sudo -u keleustes keleustes --data /var/lib/keleustes token new --label build-box
```

With no OS keyring for the service user, the vault master key is the
0600 `vault.key` file in `dataDir`.

## From source

```sh
cargo install --locked --git https://github.com/Vulnesis/keleustes kls-app
```

Needs `protoc` (3.15 or newer) and, on Linux, `pkg-config` and the
development packages for GTK 3 and D-Bus (`libgtk-3-dev libdbus-1-dev` on
Debian and Ubuntu). Installers (deb, AppImage, pacman, dmg, msi, NSIS,
rpm) build locally with `just package` and `just package-rpm`; releases
do not ship them yet.

## Uninstall

| Method | Command |
|---|---|
| Shell installer | `rm "$(command -v keleustes)"` |
| Homebrew | `brew uninstall keleustes` |
| Scoop | `scoop uninstall keleustes` |
| Nix | `nix profile remove keleustes` |

Data stays in the platform data directory (`~/.local/share/keleustes` on
Linux, `~/Library/Application Support/is.vulnes.keleustes` on macOS,
`%APPDATA%\vulnes\keleustes\data` on Windows) until you remove it.

## Releasing

1. Bump `[workspace.package] version` in `Cargo.toml` and commit.
2. Tag `vX.Y.Z` and push the tag. The release workflow refuses a tag that
   differs from the Cargo version.
3. `.github/workflows/release.yml` builds the five binaries natively,
   publishes them with `install.sh`, `checksums.txt` and (with a release
   key set) `checksums.txt.sig`, signs SLSA
   provenance through Vulnetix/cli's reusable `slsa-provenance.yml`, updates
   `Formula/keleustes.rb` in the tap and `bucket/keleustes.json` in the
   bucket (never to an older version), then signs the SBOM with cosign and
   publishes the release to TEA.

### Secrets and variables

| Name | Kind | Used by |
|---|---|---|
| `PACKAGES_TOKEN` | secret | `update-packages`: push to `Vulnesis/homebrew-tap` and `Vulnesis/scoop-bucket` (contents: write on both) |
| `VULNETIX_ORG_ID` | secret | `publish-tea`: the organisation the TEA release is published under |
| `VULNETIX_API_KEY` | secret | `publish-tea`: the CLI's credentials for that organisation |
| `VULNETIX_RELEASE_SIGNING_KEY` | secret | `release`: the Ed25519 private key as PEM (`openssl genpkey -algorithm ed25519`) that signs `checksums.txt` into `checksums.txt.sig` |
| `VULNETIX_RELEASE_PUBLIC_KEY` | variable | `release`: base64 of the raw 32-byte public key (`openssl pkey -in key.pem -pubout -outform DER \| tail -c 32 \| base64`) |

Signing follows the CLI's release. With the variable empty, `checksums.txt`
is not signed. With it set, the secret must be set too, and
`RELEASE_PUBLIC_KEY` in `install.sh` must hold the same value. The
`Sign checksums` step fails the release on either mismatch before anything
is published, since installers pinned to the key refuse an unsigned or
wrongly signed release. To turn signing on, set the variable and the
secret, commit the public key into `install.sh`, then tag. To rotate the
key, change all three together; installers fetched before the change keep
the old key and refuse releases signed with the new one.

The provenance and SBOM signatures are keyless (Sigstore, the job's OIDC
identity) and need no secret: the release and publish-tea jobs need
`id-token: write`, which the workflow grants.

### The Vulnetix/cli signer identity check

Provenance is signed by `Vulnetix/cli/.github/workflows/slsa-provenance.yml`,
called at the commit belai's release pins. That workflow reads the OIDC
`job_workflow_ref` and stops unless it names
`Vulnetix/cli/.github/workflows/slsa-provenance.yml@…`. If the CLI
repository now runs as `Vulnesis/cli`, the OIDC claim names `Vulnesis/cli`,
that check fails, and the release ships without `provenance.sigstore.json`
(installs still verify on the checksum and signature). The fix is in the
CLI repository: accept `Vulnesis/cli` in its `case` and in its own
verify step's identity regexp. `install.sh` already accepts either
organisation for the signer and pins the caller to
`Vulnesis/keleustes` with `--certificate-github-workflow-repository`.
