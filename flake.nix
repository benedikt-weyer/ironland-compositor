{
  description = "ironland-copositor dev environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # smithay-drm-extras pins `libdisplay-info < 0.4.0`; nixos-unstable has 0.4.0, so
    # pull the last compatible build (0.2.0) from an older channel for that one package.
    nixpkgs-old.url = "github:NixOS/nixpkgs/nixos-24.11";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, nixpkgs-old, flake-utils, crane }:
    let
      perSystemBuildInputs = pkgs: with pkgs; [
        wayland
        wayland-protocols
        libxkbcommon
        libinput
        seatd
        udev
        mesa
        libglvnd
        libgbm
        libdisplay-info
        vulkan-loader
        pixman
      ];
    in
    (flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            (final: prev: {
              libdisplay-info = (import nixpkgs-old { inherit system; }).libdisplay-info;
            })
          ];
        };

        nativeBuildInputs = with pkgs; [
          pkg-config
        ];

        buildInputs = perSystemBuildInputs pkgs;

        craneLib = crane.mkLib pkgs;
        commonArgs = { inherit nativeBuildInputs buildInputs; };
        # Compiles just the dependency graph (smithay and the rest) against a
        # dummy stub of our own crate. `cleanCargoSource` keeps only
        # Cargo.toml/Cargo.lock/*.rs, so this derivation's hash - and Nix's
        # cache of it - is untouched by editing our source (the dummy stub
        # never reads `resources/`, so stripping it here is fine).
        cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
          src = craneLib.cleanCargoSource ./.;
        });

        # Fyne (the GUI toolkit `gui-settings` uses) is a cgo package: it
        # links against GL and, on X11, Xlib/Xcursor/Xrandr/Xinerama/Xi/
        # Xxf86vm; Wayland/xkbcommon cover running it as a native Wayland
        # client instead. All of that has to be on hand at both build and
        # run time, hence buildInputs (not just nativeBuildInputs).
        guiSettingsBuildInputs = with pkgs; [
          libGL
          libglvnd
          libxkbcommon
          wayland
          libx11
          libxcursor
          libxrandr
          libxinerama
          libxi
          libxxf86vm
        ];
      in
      {
        # The real build needs the full source: `resources/*` is pulled in via
        # `include_bytes!`, which `cleanCargoSource` above would strip.
        packages.default = craneLib.buildPackage (commonArgs // {
          src = craneLib.path ./.;
          inherit cargoArtifacts;
        });

        packages.settings-gui = pkgs.buildGoModule {
          pname = "ironland-copositor-settings-gui";
          version = "0.1.0";
          src = ./gui-settings;
          vendorHash = "sha256-IhRYaTLleaHKfqmicA8rYOdiEW41J7CxLIWKld4Ez0Q=";
          nativeBuildInputs = [ pkgs.pkg-config pkgs.makeWrapper ];
          buildInputs = guiSettingsBuildInputs;
          # The compositor has no XWayland, so Fyne's default glfw backend
          # (X11-only unless told otherwise) can't create a window at all -
          # it fails with "x11: DISPLAY is missing" even when run from a
          # terminal inside the Wayland session, since it never looks at
          # WAYLAND_DISPLAY. This tag switches go-gl/glfw to its native
          # Wayland backend instead.
          tags = [ "wayland" ];
          # The appearance tab's dark-mode toggle shells out to `gsettings`
          # (see gui-settings/appearance.go) rather than linking against
          # glib, so it just needs the binary on PATH.
          postFixup = ''
            wrapProgram $out/bin/gui-settings --prefix PATH : ${pkgs.glib}/bin
          '';
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            rust-analyzer
            rustfmt
            clippy
          ] ++ nativeBuildInputs ++ buildInputs;

          RUST_SRC_PATH = "${pkgs.rust.packages.stable.rustPlatform.rustLibSrc}";
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath buildInputs;
        };

        devShells.settings-gui = pkgs.mkShell {
          packages = [ pkgs.go pkgs.gopls pkgs.pkg-config ] ++ guiSettingsBuildInputs;
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath guiSettingsBuildInputs;
          # See the `tags` comment on packages.settings-gui: the compositor
          # has no XWayland, so this always needs glfw's native-Wayland
          # backend. GOFLAGS applies the tag to `go build`/`go run`/`go
          # test` here without having to remember `-tags wayland` each time.
          GOFLAGS = "-tags=wayland";
        };
      })) // {
      nixosModules.default = import ./nix/module.nix;

      # A throwaway NixOS VM for exercising the `--tty-udev` backend: it owns
      # its (virtual) DRM/input devices directly, the way real hardware does,
      # so there is no host window manager around to steal modifier keys or
      # otherwise interfere the way testing nested under `--winit` can.
      # Run it with `scripts/run-vm`.
      nixosConfigurations.compositor-vm = nixpkgs.lib.nixosSystem {
        system = "x86_64-linux";
        modules = [
          self.nixosModules.default
          ({ pkgs, ... }: {
            system.stateVersion = "24.11";

            # Example settings for the compositor's own config system (see
            # `nix/module.nix` and `src/config.rs`): `weston-terminal` (the
            # hardcoded default before this became configurable) isn't even
            # installed in this VM, so without this override ctrl+return
            # would silently fail to spawn a terminal. Likewise the default
            # `browser` command ("brave") isn't installed below - only
            # firefox is - so super+b needs the same kind of override.
            services.ironland-copositor.settings = {
              terminal = "alacritty";
              browser = "firefox";
              keyboard.layout = "us";
            };

            users.users.dev = {
              isNormalUser = true;
              extraGroups = [ "wheel" "video" "input" "render" ];
              initialPassword = "dev";
            };
            security.sudo.wheelNeedsPassword = false;

            # A shell's power menu (or any other D-Bus client) asking logind
            # for PowerOff/Reboot (org.freedesktop.login1.Manager) gets
            # silently refused without polkit running to authorize that
            # action. Polkit's own default rules already grant
            # power-off/reboot/suspend to the active local session, so
            # enabling it is enough — no extra rule needed.
            security.polkit.enable = true;

            # Populates /run/opengl-driver, which is where Mesa's GBM backend
            # (dri_gbm.so) looks for its driver by NixOS convention. Without
            # this the udev backend fails at GBM device creation.
            hardware.graphics.enable = true;

            # greetd manages the VT/session lifecycle properly (PAM session,
            # logind registration) instead of a hand-rolled getty+shell hack.
            # tuigreet is just the login prompt; on successful auth it hands
            # off to `--cmd` as the actual session command. A real script
            # file (rather than a nested-quotes command string) sidesteps
            # both greetd's config parser and TOML's own quoting corner
            # cases.
            environment.etc."ironland-launch.sh".source = pkgs.writeShellScript "ironland-launch" ''
              ironland-copositor --tty-udev &
              compositor_pid=$!

              # The compositor doesn't run an autostart list itself, so bring
              # up molunga-shell (the Quickshell config built for this
              # compositor - see ../molunga-shell, a sibling repo with its
              # own flake) from here once its socket exists. Polling the
              # runtime dir for it is simpler than scraping stdout for the
              # "Listening on wayland socket" log line.
              # (`top_bar` in the compositor's own config is a different
              # thing now: it's the compositor's own window header bar, not
              # this shell, so it doesn't gate this.)
              #
              # molunga-shell isn't pulled in as a flake input here: it's a
              # sibling checkout, not a published one, and a relative `path:`
              # input escaping this repo's tree only resolves under
              # `--impure`. Whoever assembles the NixOS config this module
              # feeds into is expected to put molunga-shell's own flake
              # package on `PATH` (e.g. via an overlay, or by adding it to
              # `environment.systemPackages` alongside this module).
              runtime_dir="''${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
              socket=""
              for _ in $(seq 1 100); do
                socket=$(find "$runtime_dir" -maxdepth 1 -name 'wayland-*' ! -name '*.lock' 2>/dev/null | head -n1)
                [ -n "$socket" ] && break
                sleep 0.1
              done

              if [ -n "$socket" ] && command -v molunga-shell >/dev/null; then
                WAYLAND_DISPLAY=$(basename "$socket") molunga-shell &
              fi

              wait "$compositor_pid"
            '';

            services.greetd = {
              enable = true;
              settings.default_session = {
                command = "${pkgs.tuigreet}/bin/tuigreet --remember --remember-session --cmd /etc/ironland-launch.sh";
                user = "greeter";
              };
            };

            # Plain console only: no display manager, no host compositor of any
            # kind, so the udev backend has an uncontested seat and DRM master.
            services.xserver.enable = false;

            # The compositor is built on the host (see `packages.default`
            # above) and only the resulting binary closure lands in the VM —
            # no cargo/rustc/cc/pkg-config needed here at all.
            environment.systemPackages = [
              self.packages.x86_64-linux.default
              self.packages.x86_64-linux.settings-gui
              pkgs.alacritty
              pkgs.firefox
              pkgs.blueman
              pkgs.quickshell
              # molunga-shell (../molunga-shell) isn't wired in as a flake
              # input here - see the comment above the launch script's
              # `command -v molunga-shell` check - so this test VM falls
              # back to a bare `quickshell` with no shell config at all.
              # Build molunga-shell's own flake and add its package here to
              # exercise it in this VM.
              # Gives the launcher (and molunga-shell's dock) a
              # "Compositor Settings" entry for the Fyne GUI above, rather
              # than requiring it to be run by exact binary name.
              (pkgs.makeDesktopItem {
                name = "ironland-copositor-settings";
                desktopName = "Compositor Settings";
                comment = "Configure ironland-copositor's keyboard layout and shortcuts";
                exec = "${self.packages.x86_64-linux.settings-gui}/bin/gui-settings";
                icon = "preferences-desktop-keyboard";
                categories = [ "Settings" ];
              })
            ];

            # Still needed at runtime: some of these libraries (GL/EGL/Vulkan
            # loaders, mesa drivers) are dlopen'd rather than linked, so no
            # RPATH baked into the binary covers them.
            environment.variables.LD_LIBRARY_PATH =
              pkgs.lib.makeLibraryPath (perSystemBuildInputs pkgs);

            virtualisation.vmVariant = {
              virtualisation.memorySize = 4096;
              virtualisation.cores = 4;
              virtualisation.graphics = true;
              # Not `-vga virtio`: that's a legacy-VGA-compatible variant of
              # virtio-gpu that doesn't properly support the atomic KMS API
              # smithay requires, causing "unable to become drm master" /
              # atomic-commit EINVAL errors. A plain virtio-gpu device (no
              # VGA compatibility shim) does support atomic modesetting.
              #
              # The `-gl` variant (with a GL-enabled display) is needed on top
              # of that: plain virtio-gpu-pci exposes no DRM render node at
              # all (GBM allocation fails with NoRenderNode), only the `-gl`
              # (virgl) variant registers one. This works fine with pure
              # software rendering on the host, no real GPU passthrough
              # needed.
              virtualisation.qemu.options = [
                "-vga none"
                "-device virtio-gpu-gl-pci"
                "-display gtk,gl=on"
              ];
              # Handy for looking at source/config from inside the VM; not
              # needed to run the compositor itself, which is preinstalled.
              virtualisation.sharedDirectories.project = {
                # Set by scripts/run-vm; falls back to this repo's checkout
                # location for a manual `nix build --impure`.
                source = let v = builtins.getEnv "IRONLAND_VM_PROJECT_DIR"; in
                  if v != "" then v else "/mnt/local-storage/git/low-level/ironland-copositor";
                target = "/home/dev/project";
              };
            };
          })
        ];
      };
    };
}
