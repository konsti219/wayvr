{
  description = "Standalone WayVR flake for local development and packaging";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    crane,
    rust-overlay,
    ...
  }:
    flake-utils.lib.eachSystem ["x86_64-linux" "aarch64-linux"] (system: let
      overlays = [(import rust-overlay)];
      pkgs = import nixpkgs {
        inherit system overlays;
      };
      lib = pkgs.lib;
      # withOpenVR = system != "aarch64-linux";

      rustToolchain = pkgs.rust-bin.stable.latest.default.override {
        extensions = [
          "clippy"
          "rust-src"
          "rustfmt"
        ];
      };

      craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
      src = ./.;

      features = lib.concatStringsSep "," (
        [
          "openxr"
          "osc"
          "x11"
          "wayland"
          "feat-monado-metrics"
        ]
        # ++ lib.optionals withOpenVR ["openvr"]
      );

      commonArgs = {
        inherit src;
        strictDeps = true;
        pname = "wayvr";
        version = "26.2.1";

        cargoExtraArgs = "--package wayvr --no-default-features --features ${features}";

        nativeBuildInputs = [
          pkgs.cmake
          pkgs.pkg-config
          pkgs.rustPlatform.bindgenHook
        ];

        buildInputs =
          [
            pkgs.alsa-lib
            pkgs.dav1d
            pkgs.dbus
            # libinput + udev, for the `input` crate used by input capture
            pkgs.libinput
            pkgs.libx11
            pkgs.libxext
            pkgs.libxrandr
            pkgs.libxcb
            pkgs.libxkbcommon
            pkgs.onnxruntime
            pkgs.openssl
            pkgs.openxr-loader
            pkgs.pipewire
            pkgs.shaderc
            pkgs.systemdLibs
            pkgs.vulkan-headers
            pkgs.vulkan-loader
          ]
          # ++ lib.optionals withOpenVR [pkgs.openvr]
          ;

        env.SHADERC_LIB_DIR = "${lib.getLib pkgs.shaderc}/lib";
        env.CMAKE_ARGS = "-DCMAKE_POLICY_VERSION_MINIMUM=3.5";
        # Force ort-sys to use the system ONNX Runtime from nixpkgs
        env.ORT_STRATEGY = "system";
        env.ORT_LIB_LOCATION = "${pkgs.onnxruntime}/lib";
        env.ORT_PREFER_DYNAMIC_LINK = "1";

        # libspa-sys and pipewire-sys write their bindgen output beside their
        # sources, but Crane's vendored dependencies live in the read-only Nix
        # store. Rebuild the vendored source dir with writable copies of those
        # crates (symlinking everything else) and point cargo at it.
        preBuild = ''
          vendorSrc="$(dirname "$(find -L "$cargoVendorDir" -mindepth 2 -maxdepth 2 -type d -name 'libspa-sys-*' | head -n1)")"
          writableVendor="$TMPDIR/writable-vendor"
          mkdir -p "$writableVendor"
          for entry in "$vendorSrc"/*; do
            name="$(basename "$entry")"
            case "$name" in
              libspa-sys-*|pipewire-sys-*)
                cp -aL "$entry" "$writableVendor/$name"
                chmod -R u+w "$writableVendor/$name"
                ;;
              *)
                ln -sn "$(readlink -f "$entry")" "$writableVendor/$name"
                ;;
            esac
          done
          substituteInPlace "$CARGO_HOME/config.toml" \
            --replace-fail "$vendorSrc" "$writableVendor"
        '';

        postPatch = ''
          if [[ -f wlx-common/src/steam.rs ]]; then
            substituteInPlace wlx-common/src/steam.rs \
              --replace-fail 'Command::new("pkill")' 'Command::new("${lib.getExe' pkgs.procps "pkill"}")'
          fi
          if [[ -f wayvr/src/gui/panel/button.rs ]]; then
            substituteInPlace wayvr/src/gui/panel/button.rs \
              --replace-fail 'Command::new("wivrnctl")' 'Command::new("${lib.getExe' pkgs.wivrn "wivrnctl"}")'
          fi
          if [[ -f wayvr/src/overlays/watch.rs ]]; then
            substituteInPlace wayvr/src/overlays/watch.rs \
              --replace-fail 'command_output("lact"' 'command_output("${lib.getExe pkgs.lact}"' \
              --replace-fail 'Command::new("lact")' 'Command::new("${lib.getExe pkgs.lact}")'
          fi
        '';

        # postPatch = ''
        #   substituteAllInPlace dash-frontend/src/util/pactl_wrapper.rs \
        #     --replace-fail '"pactl"' '"${lib.getExe' pkgs.pulseaudio "pactl"}"'

        #   # steam_utils also calls xdg-open as well as steam. Those should probably be pulled from the environment
        #   substituteInPlace dash-frontend/src/util/steam_utils.rs \
        #     --replace-fail '"pkill"' '"${lib.getExe' pkgs.procps "pkill"}"'
        # '';
      };

      cargoArtifacts = craneLib.buildDepsOnly commonArgs;

      wayvrPkg = craneLib.buildPackage (
        commonArgs
        // {
          inherit cargoArtifacts;

          postInstall = ''
            install -D wayvr/wayvr.desktop -t $out/share/applications
            install -D wayvr/wayvr.svg -t $out/share/icons/hicolor/scalable/apps
          '';

          meta = {
            description = "Your way to enjoy VR on Linux! Access your Wayland/X11 desktop from SteamVR/Monado (OpenVR+OpenXR support)";
            homepage = "https://github.com/wlx-team/wayvr";
            license = with lib.licenses; [
              gpl3Only
              mit
            ];
            platforms = lib.platforms.linux;
            mainProgram = "wayvr";
          };
        }
      );

      wivrnMonadoMetrics = pkgs.wivrn.overrideAttrs (finalAttrs: oldAttrs: {
        version = "26.6";

        # WiVRn 26.6's cmake/CompileGLSL.cmake embeds shaders via `hexdump`.
        nativeBuildInputs = (oldAttrs.nativeBuildInputs or []) ++ [
          pkgs.unixtools.hexdump
        ];

        # WiVRn 26.6's dashboard now requires the kirigami-addons formcard QML module.
        buildInputs = (oldAttrs.buildInputs or []) ++ [
          pkgs.kdePackages.kirigami-addons
        ];

        src = pkgs.fetchFromGitHub {
          owner = "wivrn";
          repo = "wivrn";
          rev = "v${finalAttrs.version}";
          hash = "sha256-0RvQnaxASPcv3JkEp1OON/n4C9qEAAJ8R7m2FKPlVK0=";
        };

        # WiVRn 26.6's GitVersion.cmake requires GIT_COMMIT at build time, which
        # can't be inferred from the (gitless) nix source. v26.6 tag commit:
        cmakeFlags = (oldAttrs.cmakeFlags or []) ++ [
          "-DGIT_COMMIT=f8841585ebcc413cd2879da4d8acb2bddea1dddc"
        ];

        # NOTE: wivrn-comp-target-gpu-metrics.patch was dropped for WiVRn 26.6:
        # the compositor refactor removed server/driver/wivrn_comp_target.cpp, and
        # the SystemGpuInfo record it produced is unused by wayvr (only SessionFrame,
        # emitted by the app_pacer override below, is consumed).
        patches = (oldAttrs.patches or []) ++ [
          ./nix/wivrn-metrics-init.patch
        ];
        postPatch = (oldAttrs.postPatch or "") + ''
          cp ${./nix/wivrn-app-pacer-metrics/app_pacer.h} server/driver/app_pacer.h
          cp ${./nix/wivrn-app-pacer-metrics/app_pacer.cpp} server/driver/app_pacer.cpp
        '';

        # Monado source revision pinned by WiVRn v26.6 (see its monado-rev file),
        # with WiVRn's own monado patches plus our metrics MR applied.
        monado = pkgs.applyPatches {
          name = "monado-with-metrics";
          src = pkgs.applyPatches {
            src = pkgs.fetchFromGitLab {
              domain = "gitlab.freedesktop.org";
              owner = "monado";
              repo = "monado";
              rev = "1b526bb3a0ff326ecd05af4c2c541407f53c6d4b";
              hash = "sha256-SzuCQ1uX15vFGwGt3gswlVF2Su8sIND4R3tsTJ4T1LY=";
            };
            postPatch = ''
              ${finalAttrs.src}/patches/apply.sh ${finalAttrs.src}/patches/monado/*
            '';
          };
          # Monado metrics MR 2484, vendored with two hunks rebased onto the
          # monado revision shipped by WiVRn 26.6 (XRT_ERROR_OUT_OF_MEMORY moved
          # to -45, libmonado.def export reordered past WiVRn's chroma-key line).
          patches = [
            ./nix/wivrn-monado-mr2484.patch
          ];
          # Fail if any patch fails
          patchFlags = ["-p1" "-F0"];
        };
      });
    in {
      packages = {
        default = wayvrPkg;
        wayvr = wayvrPkg;
        wivrn-monado-metrics = wivrnMonadoMetrics;
      };

      apps.default = {
        type = "app";
        program = "${wayvrPkg}/bin/wayvr";
      };

      apps.wivrn-monado-metrics = {
        type = "app";
        program = "${wivrnMonadoMetrics}/bin/wivrn-server";
      };

      devShells.default = pkgs.mkShell {
        inputsFrom = [wayvrPkg];
        packages = [
          rustToolchain
        ];

        shellHook = ''
          export RUST_SRC_PATH="${rustToolchain}/lib/rustlib/src/rust/library"
          export SHADERC_LIB_DIR="${lib.getLib pkgs.shaderc}/lib"
          export CMAKE_ARGS="-DCMAKE_POLICY_VERSION_MINIMUM=3.5"
          export ORT_STRATEGY="system"
          export ORT_LIB_LOCATION="${pkgs.onnxruntime}/lib"
          export ORT_PREFER_DYNAMIC_LINK="1"
        '';
      };

      formatter = pkgs.nixpkgs-fmt;
    });
}
