#!/bin/sh
#
# Keleustes installer.
#
#   curl -fsSL https://raw.githubusercontent.com/Vulnesis/keleustes/main/install.sh | sh
#
# Downloads the release binary for this platform, verifies it against the
# release checksums (and, with cosign on PATH, the signed SLSA provenance),
# and installs it. Run with --help for options.

set -e

INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
VERSION="${VERSION:-latest}"
BINARY_NAME="keleustes"
GITHUB_REPO="${KELEUSTES_REPO:-Vulnesis/keleustes}"
GITHUB_BASE="https://github.com/${GITHUB_REPO}/releases"
# Overrides where the binary, checksums.txt and the provenance bundle are
# fetched from (a directory URL, file:// works with curl). For mirrors and
# for testing the installer against a local build; VERSION is then only a label.
DOWNLOAD_BASE="${KELEUSTES_DOWNLOAD_BASE:-}"
INSECURE_SKIP_VERIFY="${KELEUSTES_INSECURE_SKIP_VERIFY:-0}"
REQUIRE_PROVENANCE="${KELEUSTES_REQUIRE_PROVENANCE:-0}"
REQUIRE_SIGNATURE="${KELEUSTES_REQUIRE_SIGNATURE:-0}"

# The release signing key, as the CLI pins it: the base64 of the raw 32-byte
# Ed25519 public key, the repository variable VULNETIX_RELEASE_PUBLIC_KEY.
# The release workflow signs checksums.txt with the matching private key
# (secret VULNETIX_RELEASE_SIGNING_KEY) into checksums.txt.sig, the base64 of
# the raw 64-byte signature, and refuses to publish when this value and the
# variable differ. Empty: releases are unsigned and no signature is checked.
RELEASE_PUBLIC_KEY="Xo9QjUnzvYeW1ifTmxORoEjP+koOJc3NSd0gXGGk0W4="
SIGNATURE_ASSET="checksums.txt.sig"

# The provenance bundle and its signer, as Vulnetix/cli's reusable
# slsa-provenance.yml produces them (belai's release signs through the same
# workflow): one Sigstore bundle per release whose SLSA v1 statement has every
# line of checksums.txt as a subject. The certificate names the reusable
# workflow at the ref the caller pinned (a commit SHA, or a branch or tag);
# the repository claim pins the caller, this repository.
PROVENANCE_ASSET="provenance.sigstore.json"
PROVENANCE_TYPE="slsaprovenance1"
PROVENANCE_ISSUER="https://token.actions.githubusercontent.com"
PROVENANCE_IDENTITY_REGEXP='^https://github\.com/(Vulnetix|Vulnesis)/cli/\.github/workflows/slsa-provenance\.yml@([0-9a-f]{40}|refs/(heads/main|tags/v[0-9]+\.[0-9]+\.[0-9]+.*))$'

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

while [ $# -gt 0 ]; do
  case "$1" in
    --install-dir)
      INSTALL_DIR="$2"
      shift 2
      ;;
    --version)
      VERSION="$2"
      shift 2
      ;;
    --insecure-skip-verify)
      INSECURE_SKIP_VERIFY=1
      shift
      ;;
    --require-provenance)
      REQUIRE_PROVENANCE=1
      shift
      ;;
    --require-signature)
      REQUIRE_SIGNATURE=1
      shift
      ;;
    --help)
      cat <<EOF
Usage: install.sh [options]

Options:
  --install-dir DIR       Installation directory (default: /usr/local/bin)
  --version VERSION       Version to install, e.g. v0.1.0 (default: latest)
  --require-provenance    Fail unless cosign is on PATH and the release's signed
                          SLSA provenance verifies for the downloaded binary
  --require-signature     Fail unless an Ed25519-capable openssl verifies
                          ${SIGNATURE_ASSET} against the pinned release key
  --insecure-skip-verify  Install without checking the signature, the SHA-256
                          checksum or the provenance. Not recommended
  --help                  Show this message

Verification:
  With a release key pinned in this script, checksums.txt must carry a valid
  Ed25519 signature (${SIGNATURE_ASSET}): a missing or bad signature stops the
  install. The check needs openssl with Ed25519 (OpenSSL 3 or newer); without
  one it is skipped with a warning, unless --require-signature is set.
  The binary is installed only when its SHA-256 matches the release's
  checksums.txt. The installer stops when it has no SHA-256 tool, cannot
  fetch checksums.txt, or finds no entry for the binary. When cosign is on
  PATH it also verifies the release's signed provenance (${PROVENANCE_ASSET})
  and stops if that fails.

Linux:
  The desktop window needs GTK 3 and, for the tray icon, libayatana-appindicator
  (or libappindicator). \`keleustes --headless\` needs GTK 3 installed but no
  display. The installer says which of them is missing.

Environment variables:
  INSTALL_DIR                        Overrides --install-dir
  VERSION                            Overrides --version
  KELEUSTES_REQUIRE_PROVENANCE=1     Same as --require-provenance
  KELEUSTES_REQUIRE_SIGNATURE=1      Same as --require-signature
  KELEUSTES_INSECURE_SKIP_VERIFY=1   Same as --insecure-skip-verify
  KELEUSTES_DOWNLOAD_BASE=URL        Fetch the assets from URL instead of the release

Examples:
  curl -fsSL https://raw.githubusercontent.com/Vulnesis/keleustes/main/install.sh | sh
  curl -fsSL https://raw.githubusercontent.com/Vulnesis/keleustes/main/install.sh | sh -s -- --install-dir ~/.local/bin
  curl -fsSL https://raw.githubusercontent.com/Vulnesis/keleustes/main/install.sh | sh -s -- --version v0.1.0
  curl -fsSL https://raw.githubusercontent.com/Vulnesis/keleustes/main/install.sh | sh -s -- --require-provenance
EOF
      exit 0
      ;;
    *)
      echo "error: unknown option: $1" >&2
      echo "Run with --help for usage." >&2
      exit 1
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Environment detection
# ---------------------------------------------------------------------------

detect_os() {
  case "$(uname -s 2>/dev/null)" in
    Linux)                echo "linux" ;;
    Darwin)               echo "darwin" ;;
    CYGWIN*|MINGW*|MSYS*) echo "windows" ;;
    *)
      echo "error: unsupported OS: $(uname -s)" >&2
      exit 1
      ;;
  esac
}

# Releases cover linux amd64/arm64, darwin amd64/arm64 and windows amd64.
# Anything else is named here so the failure says what is missing rather than
# 404ing on an asset URL later.
detect_arch() {
  case "$(uname -m 2>/dev/null)" in
    x86_64|amd64)  echo "amd64" ;;
    arm64|aarch64) echo "arm64" ;;
    *)
      echo "error: unsupported architecture: $(uname -m)" >&2
      echo "error: Keleustes releases cover amd64 and arm64. Build from source:" >&2
      echo "error:   cargo install --git https://github.com/${GITHUB_REPO} kls-app" >&2
      exit 1
      ;;
  esac
}

detect_downloader() {
  if command -v curl >/dev/null 2>&1; then
    echo "curl"
  elif command -v wget >/dev/null 2>&1; then
    echo "wget"
  else
    echo "error: curl or wget is required" >&2
    exit 1
  fi
}

# Ordered most to least commonly available.
detect_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    echo "sha256sum"
  elif command -v shasum >/dev/null 2>&1; then
    echo "shasum"
  elif command -v sha256 >/dev/null 2>&1; then
    echo "sha256"
  elif command -v openssl >/dev/null 2>&1; then
    echo "openssl"
  elif command -v python3 >/dev/null 2>&1; then
    echo "python3"
  elif command -v rhash >/dev/null 2>&1; then
    echo "rhash"
  elif command -v certutil >/dev/null 2>&1; then
    echo "certutil"
  else
    echo "none"
  fi
}

compute_sha256() {
  case "$2" in
    sha256sum) sha256sum "$1" | awk '{print $1}' ;;
    shasum)    shasum -a 256 "$1" | awk '{print $1}' ;;
    sha256)    sha256 -q "$1" ;;
    openssl)   openssl dgst -sha256 "$1" | awk '{print $NF}' ;;
    python3)   python3 -c "import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],'rb').read()).hexdigest())" "$1" ;;
    rhash)     rhash --sha256 --printf='%h\n' "$1" ;;
    certutil)  certutil -hashfile "$1" SHA256 2>/dev/null | grep -v "^SHA256" | grep -v "CertUtil:" | tr -d ' \r\n' ;;
  esac
}

check_existing() {
  if [ -f "$1" ]; then
    existing_ver="$("$1" --version 2>/dev/null | head -1 || true)"
    echo "info: existing binary found: ${existing_ver:-unknown version}"
    echo "info: will be overwritten at $1"
  fi
}

resolve_install_dir() {
  dir="$1"
  if mkdir -p "$dir" 2>/dev/null && [ -w "$dir" ]; then
    echo "$dir"
    return
  fi
  fallback="$HOME/.local/bin"
  echo "warn: $dir is not writable, falling back to $fallback" >&2
  mkdir -p "$fallback"
  echo "$fallback"
}

download() {
  if [ "$3" = "curl" ]; then
    # --retry-all-errors is what makes this retry a TRUNCATED transfer (curl
    # exit 18), which plain --retry treats as a hard failure.
    curl -fsSL --retry 5 --retry-delay 2 --retry-all-errors "$1" -o "$2"
  else
    wget -q --tries=5 --timeout=30 --waitretry=2 "$1" -O "$2"
  fi
}

# fetch_optional downloads a file a release may not have, without retrying a
# 404 five times.
fetch_optional() {
  if [ "$3" = "curl" ]; then
    curl -fsSL --retry 3 --retry-delay 2 "$1" -o "$2" 2>/dev/null
  else
    wget -q --tries=3 --timeout=30 "$1" -O "$2" 2>/dev/null
  fi
}

# resolve_latest_tag turns the /releases/latest redirect into a concrete tag,
# with no token and no API rate limit. Prints nothing when it cannot resolve.
resolve_latest_tag() {
  url=""
  if command -v curl >/dev/null 2>&1; then
    url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' --retry 2 \
      "${GITHUB_BASE}/latest" 2>/dev/null || true)
  elif command -v wget >/dev/null 2>&1; then
    url=$(wget -q --spider --server-response --max-redirect=10 \
      "${GITHUB_BASE}/latest" 2>&1 \
      | awk '/^  Location: /{print $2}' | tail -n1)
  fi
  case "$url" in
    */tag/v*) printf '%s' "${url##*/tag/}" ;;
    *) printf '' ;;
  esac
}

# expected_size reports the asset's size from the server, or nothing, so an
# interrupted download is told apart from an integrity failure.
expected_size() {
  case "$1" in
    http://*|https://*) ;;
    *) return 0 ;;
  esac
  if [ "$2" = "curl" ]; then
    curl -fsSLI --retry 2 "$1" 2>/dev/null \
      | tr -d '\r' | awk 'tolower($1) == "content-length:" { print $2 }' | tail -n1
  else
    wget -q --spider --server-response "$1" 2>&1 \
      | tr -d '\r' | awk 'tolower($1) == "content-length:" { print $2 }' | tail -n1
  fi
}

verify_size() {
  path="$1"
  want="${2:-}"
  if [ ! -f "$path" ]; then
    echo "error: downloaded file not found at $path" >&2
    exit 1
  fi
  size=$(wc -c < "$path" 2>/dev/null || echo 0)
  size=$((size + 0))
  if [ "$size" -lt 1024 ]; then
    echo "error: downloaded file is suspiciously small (${size} bytes): the download may have failed" >&2
    exit 1
  fi
  if [ -n "$want" ] && [ "$want" -gt 0 ] 2>/dev/null && [ "$size" -ne "$want" ]; then
    echo "error: download is incomplete: got ${size} bytes, server declared ${want}" >&2
    echo "error: this is a network problem, not a corrupt release. Please run the installer again." >&2
    exit 1
  fi
  echo "info: download size=${size} bytes"
}

# refuse_unverified is for "cannot check", never for "checked and wrong".
refuse_unverified() {
  echo "error: $1, refusing to install" >&2
  echo "error: to install without verification (not recommended), re-run with" >&2
  echo "error:   --insecure-skip-verify, or set KELEUSTES_INSECURE_SKIP_VERIFY=1" >&2
  exit 1
}

# verify_checksum fails closed: no SHA-256 tool, no checksums.txt or no entry
# for the asset each stop the install, as a mismatch does.
verify_checksum() {
  asset="$1"
  tmp_binary="$2"
  checksums_url="$3"
  downloader="$4"
  sha_tool="$5"

  if [ "$sha_tool" = "none" ]; then
    refuse_unverified "no SHA-256 tool found (sha256sum, shasum, sha256, openssl, python, rhash or certutil)"
  fi

  echo "info: fetching $checksums_url"
  if ! download "$checksums_url" "$TMP_CHECKSUMS" "$downloader" 2>&1; then
    refuse_unverified "could not fetch checksums.txt"
  fi
  verify_signature "$TMP_CHECKSUMS" "${checksums_url}.sig" "$downloader"

  expected=$(grep " \*\{0,1\}${asset}\$" "$TMP_CHECKSUMS" | awk '{print $1}' | head -n1)
  if [ -z "$expected" ]; then
    refuse_unverified "asset '${asset}' has no entry in checksums.txt"
  fi
  echo "info: expected sha256=$expected"

  actual=$(compute_sha256 "$tmp_binary" "$sha_tool")
  echo "info: actual   sha256=$actual"

  if [ "$actual" != "$expected" ]; then
    echo "error: checksum mismatch, refusing to install" >&2
    echo "error:   expected: $expected" >&2
    echo "error:   actual:   $actual" >&2
    echo "error:   size:     $(wc -c < "$tmp_binary" 2>/dev/null || echo unknown) bytes" >&2
    echo "error: if the size above looks short, the download was interrupted: run the installer again." >&2
    echo "error: if it is the full size, do not install this binary. Report it at" >&2
    echo "error:   https://github.com/${GITHUB_REPO}/issues" >&2
    exit 1
  fi
  echo "info: checksum verified ok"
}

# b64decode decodes base64 on stdin. openssl is tried first since the
# signature check needs it anyway; base64 -d covers GNU, busybox and macOS 13+.
b64decode() {
  if command -v openssl >/dev/null 2>&1; then
    openssl base64 -d -A
  else
    base64 -d
  fi
}

# write_ed25519_pub turns a base64 raw Ed25519 key into the SubjectPublicKeyInfo
# DER openssl reads: the fixed 12-byte prefix the release workflow uses, then
# the 32 key bytes.
write_ed25519_pub() {
  { printf '\060\052\060\005\006\003\053\145\160\003\041\000'; printf '%s' "$1" | b64decode; } > "$2"
}

# ed25519_verify_capable reports whether openssl can verify Ed25519 over a raw
# message (OpenSSL 3+; macOS's LibreSSL cannot). It checks RFC 8032 test 1
# (empty message), so a "no" here is the tool, never a release signature.
ed25519_verify_capable() {
  command -v openssl >/dev/null 2>&1 || return 1
  d="$TMP_SIGDIR/selftest"
  mkdir -p "$d"
  : > "$d/msg"
  write_ed25519_pub "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=" "$d/pub.der"
  printf '%s' "5VZDAMNgrHKQhuLMgG6CioSHfx645dl02HPgZSJJAVVfuIIVkKM7rMYeOXAc+bRr0lv18FlbviRlUUFDjnoQCw==" | b64decode > "$d/sig"
  openssl pkeyutl -verify -pubin -keyform DER -inkey "$d/pub.der" -rawin \
    -in "$d/msg" -sigfile "$d/sig" >/dev/null 2>&1
}

# verify_signature checks checksums.txt against the pinned release key before
# a single digest in it is trusted. With a key pinned, a missing or bad
# signature always stops the install. A tool that cannot check is a warning,
# as cosign's absence is for provenance, unless --require-signature is set.
verify_signature() {
  checksums="$1"
  sig_url="$2"
  downloader="$3"

  if [ -z "$RELEASE_PUBLIC_KEY" ]; then
    if [ "$REQUIRE_SIGNATURE" = "1" ]; then
      echo "error: --require-signature is set but this installer pins no release key, refusing to install" >&2
      exit 1
    fi
    return
  fi

  if ! ed25519_verify_capable; then
    if [ "$REQUIRE_SIGNATURE" = "1" ]; then
      echo "error: --require-signature is set but no openssl here verifies Ed25519 (OpenSSL 3 or newer), refusing to install" >&2
      exit 1
    fi
    echo "warn: no Ed25519-capable openssl (OpenSSL 3 or newer), skipping the ${SIGNATURE_ASSET} check" >&2
    return
  fi

  echo "info: fetching $sig_url"
  if ! fetch_optional "$sig_url" "$TMP_SIGDIR/sig.b64" "$downloader" || [ ! -s "$TMP_SIGDIR/sig.b64" ]; then
    echo "error: this release has no ${SIGNATURE_ASSET} but releases are signed, refusing to install" >&2
    echo "error: do not install this binary. Report it at https://github.com/${GITHUB_REPO}/issues" >&2
    exit 1
  fi
  write_ed25519_pub "$RELEASE_PUBLIC_KEY" "$TMP_SIGDIR/release-pub.der"
  if ! b64decode < "$TMP_SIGDIR/sig.b64" > "$TMP_SIGDIR/sig.raw" 2>/dev/null \
     || ! openssl pkeyutl -verify -pubin -keyform DER -inkey "$TMP_SIGDIR/release-pub.der" -rawin \
          -in "$checksums" -sigfile "$TMP_SIGDIR/sig.raw" >/dev/null 2>&1; then
    echo "error: checksums.txt signature does not verify against the release key, refusing to install" >&2
    echo "error: do not install this binary. Report it at https://github.com/${GITHUB_REPO}/issues" >&2
    exit 1
  fi
  echo "info: checksums.txt signature verified ok (Ed25519 release key)"
}

check_provenance_tooling() {
  if [ "$REQUIRE_PROVENANCE" = "1" ] && ! command -v cosign >/dev/null 2>&1; then
    echo "error: --require-provenance is set but cosign is not on PATH" >&2
    echo "error: install cosign (https://docs.sigstore.dev/cosign/system_config/installation/) and run the installer again" >&2
    exit 1
  fi
}

# verify_provenance checks the release's signed SLSA provenance names this
# exact binary. A bundle that is present and does not verify always stops the
# install; a release with no bundle installs on its checksum alone unless
# provenance is required.
verify_provenance() {
  tmp_binary="$1"
  bundle_url="$2"
  downloader="$3"

  if ! command -v cosign >/dev/null 2>&1; then
    echo "info: cosign not found, skipping provenance verification (checksum verified)"
    return
  fi

  echo "info: fetching $bundle_url"
  if ! fetch_optional "$bundle_url" "$TMP_BUNDLE" "$downloader" || [ ! -s "$TMP_BUNDLE" ]; then
    if [ "$REQUIRE_PROVENANCE" = "1" ]; then
      echo "error: this release has no ${PROVENANCE_ASSET} and --require-provenance is set, refusing to install" >&2
      echo "error: it is signed a few minutes after a release is published; try again shortly" >&2
      exit 1
    fi
    echo "warn: this release has no ${PROVENANCE_ASSET} (yet); installing on the checksum alone" >&2
    return
  fi

  if out=$(cosign verify-blob-attestation \
      --bundle "$TMP_BUNDLE" --new-bundle-format \
      --type "$PROVENANCE_TYPE" \
      --certificate-oidc-issuer "$PROVENANCE_ISSUER" \
      --certificate-identity-regexp "$PROVENANCE_IDENTITY_REGEXP" \
      --certificate-github-workflow-repository "$GITHUB_REPO" \
      "$tmp_binary" 2>&1); then
    echo "info: provenance verified ok (signed by Vulnetix/cli slsa-provenance.yml for ${GITHUB_REPO})"
    return
  fi

  echo "error: provenance verification failed, refusing to install" >&2
  printf '%s\n' "$out" | sed 's/^/error:   cosign: /' >&2
  echo "error: if cosign could not reach Sigstore (a network error above), run the installer again." >&2
  echo "error: otherwise do not install this binary. Report it at" >&2
  echo "error:   https://github.com/${GITHUB_REPO}/issues" >&2
  exit 1
}

# has_lib reports whether the dynamic loader can find a shared library.
has_lib() {
  if command -v ldconfig >/dev/null 2>&1 && ldconfig -p 2>/dev/null | grep -q "$1"; then
    return 0
  fi
  for d in /usr/lib /usr/lib64 /lib /lib64 /usr/lib/x86_64-linux-gnu /usr/lib/aarch64-linux-gnu /usr/local/lib; do
    for f in "$d"/"$1"*; do
      [ -e "$f" ] && return 0
    done
  done
  return 1
}

# linux_runtime_deps names the shared libraries the binary loads that are not
# part of a base system, with the package that brings each on this distro.
linux_runtime_deps() {
  missing_gtk=0
  missing_tray=0
  has_lib libgtk-3.so.0 || missing_gtk=1
  if ! has_lib libayatana-appindicator3.so.1 && ! has_lib libappindicator3.so.1; then
    missing_tray=1
  fi
  if [ "$missing_gtk" = 0 ] && [ "$missing_tray" = 0 ]; then
    echo "info: runtime libraries ok (gtk3, appindicator)"
    return
  fi
  echo ""
  [ "$missing_gtk" = 1 ] && echo "warn: GTK 3 (libgtk-3.so.0) not found: keleustes will not start without it"
  [ "$missing_tray" = 1 ] && echo "warn: libayatana-appindicator not found: the window runs without a tray icon"
  if command -v apt-get >/dev/null 2>&1; then
    echo "      sudo apt-get install libgtk-3-0 libayatana-appindicator3-1"
  elif command -v dnf >/dev/null 2>&1; then
    echo "      sudo dnf install gtk3 libayatana-appindicator-gtk3"
  elif command -v pacman >/dev/null 2>&1; then
    echo "      sudo pacman -S gtk3 libayatana-appindicator"
  elif command -v zypper >/dev/null 2>&1; then
    echo "      sudo zypper install gtk3 libayatana-appindicator3-1"
  elif command -v apk >/dev/null 2>&1; then
    echo "      sudo apk add gtk+3.0 libayatana-appindicator"
  else
    echo "      install GTK 3 and libayatana-appindicator with your package manager"
  fi
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
  echo "keleustes installer"
  echo "-------------------"

  OS=$(detect_os)
  ARCH=$(detect_arch)
  PLATFORM="${OS}-${ARCH}"
  DOWNLOADER=$(detect_downloader)
  SHA_TOOL=$(detect_sha256)
  echo "info: os=$OS arch=$ARCH platform=$PLATFORM downloader=$DOWNLOADER sha256=$SHA_TOOL"

  if [ "$PLATFORM" = "windows-arm64" ]; then
    echo "info: no windows-arm64 build; installing windows-amd64, which runs under emulation"
    ARCH="amd64"
    PLATFORM="windows-amd64"
  fi

  if [ "$INSECURE_SKIP_VERIFY" = "1" ] && { [ "$REQUIRE_PROVENANCE" = "1" ] || [ "$REQUIRE_SIGNATURE" = "1" ]; }; then
    echo "error: --insecure-skip-verify and --require-provenance or --require-signature contradict each other; pick one" >&2
    exit 1
  fi
  check_provenance_tooling
  if [ "$SHA_TOOL" = "none" ] && [ "$INSECURE_SKIP_VERIFY" != "1" ]; then
    refuse_unverified "no SHA-256 tool found (sha256sum, shasum, sha256, openssl, python, rhash or certutil)"
  fi

  INSTALL_DIR=$(resolve_install_dir "$INSTALL_DIR")
  EXT=""
  [ "$OS" = "windows" ] && EXT=".exe"
  BINARY_PATH="${INSTALL_DIR}/${BINARY_NAME}${EXT}"
  echo "info: install dir=$INSTALL_DIR"

  check_existing "$BINARY_PATH"

  ASSET="${BINARY_NAME}-${PLATFORM}${EXT}"

  # `latest` is resolved to a concrete tag ONCE and both downloads are pinned
  # to it: through the /latest/ alias a release landing mid-download would
  # pair one release's bytes with the next one's digest.
  if [ -z "$DOWNLOAD_BASE" ] && [ "$VERSION" = "latest" ]; then
    RESOLVED=$(resolve_latest_tag)
    if [ -n "$RESOLVED" ]; then
      VERSION="$RESOLVED"
    else
      echo "warn: could not resolve the latest tag; falling back to the /latest/ alias." >&2
      echo "warn: if a release lands mid-download this can report a checksum mismatch; retry if it does." >&2
    fi
  fi

  if [ -n "$DOWNLOAD_BASE" ]; then
    RELEASE_URL="${DOWNLOAD_BASE%/}"
  elif [ "$VERSION" = "latest" ]; then
    RELEASE_URL="${GITHUB_BASE}/latest/download"
  else
    RELEASE_URL="${GITHUB_BASE}/download/${VERSION}"
  fi
  DOWNLOAD_URL="${RELEASE_URL}/${ASSET}"
  CHECKSUMS_URL="${RELEASE_URL}/checksums.txt"
  BUNDLE_URL="${RELEASE_URL}/${PROVENANCE_ASSET}"

  echo "info: version=$VERSION asset=$ASSET"
  echo "info: binary    $DOWNLOAD_URL"
  echo "info: checksums $CHECKSUMS_URL"

  # One trap removes every temporary file, so a refusal at any step leaves
  # nothing behind in the install directory.
  TMP_FILE=$(mktemp "${INSTALL_DIR}/.${BINARY_NAME}.XXXXXX" 2>/dev/null || mktemp)
  TMP_CHECKSUMS=$(mktemp)
  TMP_BUNDLE=$(mktemp)
  TMP_SIGDIR=$(mktemp -d)
  trap 'rm -f "$TMP_FILE" "$TMP_CHECKSUMS" "$TMP_BUNDLE"; rm -rf "$TMP_SIGDIR"' EXIT
  trap 'exit 1' INT TERM

  echo "info: fetching $DOWNLOAD_URL"
  EXPECT_BYTES=$(expected_size "$DOWNLOAD_URL" "$DOWNLOADER" || true)
  download "$DOWNLOAD_URL" "$TMP_FILE" "$DOWNLOADER"
  verify_size "$TMP_FILE" "$EXPECT_BYTES"
  if [ "$INSECURE_SKIP_VERIFY" = "1" ]; then
    echo "warn: --insecure-skip-verify is set: installing WITHOUT checking the signature, checksum or provenance" >&2
  else
    verify_checksum "$ASSET" "$TMP_FILE" "$CHECKSUMS_URL" "$DOWNLOADER" "$SHA_TOOL"
    verify_provenance "$TMP_FILE" "$BUNDLE_URL" "$DOWNLOADER"
  fi

  chmod 755 "$TMP_FILE"
  mv "$TMP_FILE" "$BINARY_PATH"
  echo "info: installed to $BINARY_PATH"

  INSTALLED_VER=$("$BINARY_PATH" --version 2>/dev/null | head -1 || true)
  if [ -n "$INSTALLED_VER" ]; then
    echo "info: verified: $INSTALLED_VER"
  else
    echo "warn: binary installed but --version produced no output"
  fi

  [ "$OS" = "linux" ] && linux_runtime_deps

  case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
      echo ""
      echo "warn: $INSTALL_DIR is not in PATH"
      echo "      add to your shell profile:"
      echo "      export PATH=\"${INSTALL_DIR}:\$PATH\""
      ;;
  esac

  echo ""
  echo "run:  $BINARY_NAME                 # open the window; serves nodes on 0.0.0.0:7443"
  echo "      $BINARY_NAME --headless      # serve nodes without a window"
  echo "      $BINARY_NAME token new       # a join token for belai's /connect"
  echo ""
  echo "docs: https://github.com/${GITHUB_REPO}/blob/main/docs/install.md"
}

main "$@"
