# Keleustes: the `keleustes` binary from crates/kls-app.
#
# The version is the workspace version in Cargo.toml, the same one the release
# workflow checks the tag against, so the flake needs no edit per release.
{
  lib,
  stdenv,
  rustPlatform,
  pkg-config,
  protobuf,
  sqlite,
  wrapGAppsHook3,
  gtk3,
  glib,
  dbus,
  libayatana-appindicator,
  libxkbcommon,
  wayland,
  libGL,
  vulkan-loader,
  libx11,
  libxcursor,
  libxrandr,
  libxi,
}:

let
  cargoToml = lib.importTOML ../Cargo.toml;

  # Libraries winit, glutin and libappindicator-sys dlopen at run time, so
  # they are not DT_NEEDED and must be put on the binary's RUNPATH by hand.
  dlopened = [
    libayatana-appindicator
    libxkbcommon
    wayland
    libGL
    vulkan-loader
    libx11
    libxcursor
    libxrandr
    libxi
  ];
in
rustPlatform.buildRustPackage {
  pname = "keleustes";
  inherit (cargoToml.workspace.package) version;

  src = lib.cleanSourceWith {
    src = ../.;
    filter =
      path: type:
      let
        base = baseNameOf path;
      in
      !(
        (type == "directory" && (base == "target" || base == ".github" || base == "nix"))
        || lib.hasSuffix ".db" base
        || base == "flake.lock"
      );
  };

  cargoLock.lockFile = ../Cargo.lock;

  cargoBuildFlags = [
    "-p"
    "kls-app"
  ];

  # The suite opens sockets, a keyring and Postgres when asked; it runs in CI
  # (`just check`), not in the sandbox.
  doCheck = false;

  nativeBuildInputs = [
    pkg-config
    protobuf
  ]
  ++ lib.optionals stdenv.hostPlatform.isLinux [ wrapGAppsHook3 ];

  # sqlite: the sqlx-macros build links libsqlite3-sys against it when the
  # Nix build environment selects the system library over the bundled one.
  buildInputs = [
    sqlite
  ]
  ++ lib.optionals stdenv.hostPlatform.isLinux (
    [
      gtk3
      glib
      dbus
    ]
    ++ dlopened
  );

  # The vendored protoc is the one prost-build would look for on PATH.
  env.PROTOC = lib.getExe' protobuf "protoc";

  postInstall = lib.optionalString stdenv.hostPlatform.isLinux ''
    install -Dm644 packaging/keleustes.desktop $out/share/applications/keleustes.desktop
    install -Dm644 packaging/icons/keleustes.svg $out/share/icons/hicolor/scalable/apps/keleustes.svg
    for s in 32 64 128 256 512; do
      install -Dm644 packaging/icons/keleustes-$s.png $out/share/icons/hicolor/''${s}x''${s}/apps/keleustes.png
    done
  '';

  postFixup = lib.optionalString stdenv.hostPlatform.isLinux ''
    patchelf --add-rpath ${lib.makeLibraryPath dlopened} $out/bin/.keleustes-wrapped 2>/dev/null \
      || patchelf --add-rpath ${lib.makeLibraryPath dlopened} $out/bin/keleustes
  '';

  meta = {
    description = "The desktop Software Factory for a fleet of belai hosts";
    homepage = "https://github.com/Vulnesis/keleustes";
    license = lib.licenses.asl20;
    mainProgram = "keleustes";
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
  };
}
