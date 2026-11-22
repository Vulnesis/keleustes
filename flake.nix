{
  description = "Keleustes: the desktop Software Factory for a fleet of belai hosts";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    {
      # pkgs.keleustes, for a system configuration that does not take the
      # package from this flake's outputs.
      overlays.default = final: _prev: {
        keleustes = final.callPackage ./nix/package.nix { };
      };

      # services.keleustes: headless, serving nodes, as a systemd service.
      nixosModules.default = import ./nix/module.nix { inherit self; };
      nixosModules.keleustes = self.nixosModules.default;
    }
    // flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        keleustes = pkgs.callPackage ./nix/package.nix { };
      in
      {
        packages = {
          inherit keleustes;
          default = keleustes;
        };

        apps = {
          keleustes = flake-utils.lib.mkApp { drv = keleustes; } // {
            meta.description = keleustes.meta.description;
          };
          default = self.apps.${system}.keleustes;
        };

        # `nix flake check` builds the package, so CI catches a native
        # dependency the derivation is missing.
        checks = {
          inherit keleustes;
        }
        // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          # The module evaluates and its unit file builds.
          nixos-module =
            (nixpkgs.lib.nixosSystem {
              inherit system;
              modules = [
                self.nixosModules.default
                {
                  boot.isContainer = true;
                  system.stateVersion = "25.11";
                  services.keleustes = {
                    enable = true;
                    advertiseAddrs = [ "10.0.0.5:7443" ];
                    openFirewall = true;
                  };
                }
              ];
            }).config.systemd.units."keleustes.service".unit;
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ keleustes ];
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
            just
            protobuf
          ];
          # What the binary dlopens (winit, glutin, the tray), for `cargo run`.
          LD_LIBRARY_PATH = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux (
            pkgs.lib.makeLibraryPath (
              with pkgs;
              [
                libayatana-appindicator
                libxkbcommon
                wayland
                libGL
                vulkan-loader
                libx11
                libxcursor
                libxrandr
                libxi
              ]
            )
          );
        };
      }
    );
}
