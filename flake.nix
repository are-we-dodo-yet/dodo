# This provides a Nix dev shell and Nix build for deno and denort binaries.
{
  description = "Deno";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    inputs@{
      nixpkgs,
      flake-parts,
      rust-overlay,
      crane,
      ...
    }:
    let
      rustyV8Targets = {
        "x86_64-linux" = "x86_64-unknown-linux-gnu";
        "aarch64-linux" = "aarch64-unknown-linux-gnu";
        "x86_64-darwin" = "x86_64-apple-darwin";
        "aarch64-darwin" = "aarch64-apple-darwin";
      };
    in
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = builtins.attrNames rustyV8Targets;

      perSystem =
        {
          system,
          pkgs,
          lib,
          config,
          ...
        }:
        let
          inherit (pkgs.stdenv.hostPlatform) isLinux;

          rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          rustDev = rust.override {
            extensions = [
              "rust-src"
              "rust-analyzer"
            ];
          };
          craneLib = (crane.mkLib pkgs).overrideToolchain rust;
          craneLibDev = (crane.mkLib pkgs).overrideToolchain rustDev;

          cargoToml = lib.importTOML ./cli/Cargo.toml;
          denoVersion = cargoToml.package.version;

          cargoLock = lib.importTOML ./Cargo.lock;
          rustyV8Version = (builtins.head (builtins.filter (p: p.name == "v8") cargoLock.package)).version;
          rustyV8Target = rustyV8Targets.${system};
          rustyV8 = pkgs.fetchurl {
            url = "https://github.com/denoland/rusty_v8/releases/download/v${rustyV8Version}/librusty_v8_release_${rustyV8Target}.a.gz";
            sha256 =
              {
                "x86_64-linux" = "sha256-tmg+mvy3f72MssOs9F001SApqyFrMyUiyVF5wS8P7Tw=";
                "aarch64-linux" = "sha256-xAGWu/GCUy5YP2W32vAZb9uzNU5vrFmzHQU5wg+THz0=";
                "x86_64-darwin" = "sha256-BMvBw2xM1UWcVASXddkoiTN8adk2+wsBH5sm8dRUYQ0=";
                "aarch64-darwin" = "sha256-pKVIREStS18Q3HvtFBI23Z2+q/IAbRRSDxm5NdTbQic=";
              }
              .${system};
          };

          commonArgs = {
            src = ./.;
            version = denoVersion;
            strictDeps = true;
            doCheck = false;

            nativeBuildInputs = with pkgs; [
              llvmPackages_20.clang
              lld_20
              llvmPackages_20.libllvm
              pkg-config
              cmake
              protobuf
            ];

            buildInputs = [ pkgs.openssl ] ++ lib.optionals isLinux [ pkgs.glib ];

            LIBCLANG_PATH = lib.makeLibraryPath [ pkgs.llvmPackages_20.clang-unwrapped.lib ];
            RUSTY_V8_ARCHIVE = rustyV8;

            outputHashes = {
              "git+https://github.com/are-we-dodo-yet/denokv#2da66c0ee618d9558a23da86348db12dd5da273d" =
                "sha256-Y+eehqBLA9PGscAg47gm1RAZniciWSElZteZiJrCO/M=";
              "git+https://github.com/are-we-dodo-yet/dprint-plugin-json?rev=68efb846b4951e47aa8ba3324a260cffaa830313#68efb846b4951e47aa8ba3324a260cffaa830313" =
                "sha256-1/EfLX1HpCql9mgNDVD4idoOwfegdb6DmRbdEY0Uub0=";
              "git+https://github.com/are-we-dodo-yet/dprint-plugin-jupyter?rev=0125264c5776bf703395d9eda56a28d43f8aed1e#0125264c5776bf703395d9eda56a28d43f8aed1e" =
                "sha256-UA+Rl0vcZRBeA0vh8esbAW8BTCJkeCVE+aV0i8e1/7U=";
              "git+https://github.com/are-we-dodo-yet/dprint-plugin-markdown?rev=b342a36cfd61dda148032e2330fea0c725ae1c84#b342a36cfd61dda148032e2330fea0c725ae1c84" =
                "sha256-9pQzo7nIFetTDYgt94XshprK+0YwJpUFI45ZAwjO2y0=";
              "git+https://github.com/are-we-dodo-yet/dprint-plugin-typescript?rev=040482df74e4d5a3494c6bcd31a460b10b220ab0#040482df74e4d5a3494c6bcd31a460b10b220ab0" =
                "sha256-8fLlMUzXXBEeE9eQl/6CyAHkzAFUivwtqDKvjelwRQk=";
              "git+https://github.com/are-we-dodo-yet/lax?branch=push-xyvrpnyxxwsn#a1753cafa02a34f679eebbd9b1e2b28f728a794d" =
                "sha256-Ts3Wsl7gPSQebM9gKScU9gqrkntO1QThTLNiWAgCzT4=";
            };

            preBuild = ''
              unset AS
            '';
          };

          buildDenoBin =
            pname:
            let
              args = commonArgs // {
                inherit pname;
                cargoExtraArgs = "--locked --bin ${pname}";
              };
            in
            craneLib.buildPackage (
              args
              // {
                cargoArtifacts = craneLib.buildDepsOnly (
                  args
                  // {
                    buildPhaseCargoCommand = "cargoWithProfile build ${args.cargoExtraArgs}";
                  }
                );

                postFixup = lib.optionalString isLinux ''
                  patchelf --add-rpath ${lib.makeLibraryPath [ pkgs.vulkan-loader ]} $out/bin/${pname}
                '';

                meta = {
                  mainProgram = pname;
                  platforms = builtins.attrNames rustyV8Targets;
                };
              }
            );
        in
        {
          _module.args.pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };

          packages = {
            deno = buildDenoBin "deno";
            denort = buildDenoBin "denort";

            default = config.packages.deno;
          };

          apps.nix-update-rusty-v8 = {
            type = "app";
            program = toString (
              pkgs.writeShellScript "update-rusty-v8" ''
                set -euo pipefail

                function update() {
                  sys=$1
                  target=$2
                  url="https://github.com/denoland/rusty_v8/releases/download/v${rustyV8Version}/librusty_v8_release_$target.a.gz"
                  hash=$(nix store prefetch-file --json "$url" | ${lib.getExe pkgs.jq} -r '.hash')
                  sed -i "s|\"$sys\" = \"sha256-[^\"]*\";|\"$sys\" = \"$hash\";|" flake.nix
                }

                ${lib.concatStringsSep "\n" (
                  lib.mapAttrsToList (sys: target: "update ${sys} ${target}") rustyV8Targets
                )}
              ''
            );
          };

          devShells.default = craneLibDev.devShell {
            packages =
              commonArgs.nativeBuildInputs
              ++ (with pkgs; [
                nodejs_22
                python3
              ]);
            inherit (commonArgs) buildInputs LIBCLANG_PATH;

            # Allow cargo to download crates
            SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";

            shellHook = ''
              # Fix invalid option errors during linking
              unset AS
            ''
            + lib.optionalString isLinux ''
              # On non-NixOS Linux, prevent Nix from setting the interpreter and rpath
              # which would make compiled binaries dependent on the Nix store.
              # See: https://matklad.github.io/2022/03/14/rpath-or-why-lld-doesnt-work-on-nixos.html
              if ! [ -e /etc/NIXOS ]; then
                set -- $NIX_LDFLAGS
                for i; do
                  shift
                  if [ "$i" = -rpath ]; then
                    shift
                  else
                    set -- "$@" "$i"
                  fi
                done
                export NIX_DYNAMIC_LINKER=$(patchelf --print-interpreter /usr/bin/env)
                export NIX_DONT_SET_RPATH=1
                export NIX_LDFLAGS="$@"
                # Nix's libclang is incompatible with system glibc (non-NixOS Linux)
                unset LIBCLANG_PATH
              else
                export LD_LIBRARY_PATH="${
                  lib.makeLibraryPath [ pkgs.vulkan-loader ]
                }''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
                export NIX_LD_LIBRARY_PATH="${
                  lib.makeLibraryPath (
                    with pkgs;
                    [
                      glib
                      nss
                      nspr
                      at-spi2-core
                      dbus
                      expat
                      libxkbcommon
                      alsa-lib
                      libgbm
                      libx11
                      libxcomposite
                      libxdamage
                      libxext
                      libxfixes
                      libxrandr
                      libxcb
                    ]
                  )
                }''${NIX_LD_LIBRARY_PATH:+:$NIX_LD_LIBRARY_PATH}"
              fi
            '';
          };
        };
    };
}
