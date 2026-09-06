{ config, lib, pkgs, ... }:

let
  cfg = config.services.ironland-copositor;
  settingsFormat = pkgs.formats.toml { };
in
{
  options.services.ironland-copositor.settings = lib.mkOption {
    type = settingsFormat.type;
    default = { };
    description = ''
      Settings for ironland-copositor, written to
      `/etc/ironland-copositor/config.toml` and read by the compositor at
      startup (a restart is needed to pick up changes). See the
      compositor's `src/config.rs` for the full schema: keyboard layout
      under `keyboard` (passed straight through to xkbcommon), the
      terminal launched by the `run_terminal` shortcut under `terminal`, the
      browser/file manager launched by `open_browser`/`open_file_manager`
      under `browser`/`file_manager`,
      `top_bar` (whether windows may get a compositor-drawn header/title
      bar for server-side decoration; off by default, so a client's request
      for one is overridden back to client-side), key bindings under
      `shortcuts` —
      only the actions you set there override the built-in defaults, so a
      partial table is fine — and
      per-monitor settings under `outputs.<connector-name>` (e.g.
      `outputs."eDP-1"`, `outputs."HDMI-A-1"`): `primary` marks the main
      monitor, `refresh_rate` selects its refresh rate in millihertz,
      `mirror_of` duplicates another output's position onto this one, and
      `position` places this output relative to another
      (`right_of`/`left_of`/`above`/`below`, each an output name) or at an
      absolute `{ x, y }`. An output not listed here is auto-placed to the
      right of the others, matching the compositor's previous behavior.
      Gaussian background blur for translucent application windows is under
      `blur`, with `enabled` (default false) and `radius` (default 12). And
      virtual desktops under `workspaces`: `mode` is `"per_monitor"`
      (default, each output has its own set) or `"combined"` (every output
      switches together); `count` is the starting number of workspaces
      (default 4); `dynamic` (default false), if true, grows the count on
      demand and prunes empty trailing workspaces automatically instead of
      keeping it fixed at `count`; `overlay` (default true) shows a row of
      dots on screen briefly whenever the active workspace changes. Switch
      workspaces with Super+Left/Right and move the focused window to an
      adjacent one with Super+Alt+Left/Right (both rebindable in
      `shortcuts` as `workspace_left`/`workspace_right` and
      `move_workspace_left`/`move_workspace_right`).
    '';
    example = lib.literalExpression ''
      {
        keyboard = {
          layout = "de";
          variant = "nodeadkeys";
        };
        terminal = "alacritty";
        shortcuts = {
          # Overrides just this one binding; every other shortcut keeps
          # its built-in default.
          open_browser = [ "super+b" ];
        };
        outputs = {
          "eDP-1".primary = true;
          "HDMI-A-1".position.right_of = "eDP-1";
          # "DP-1".mirror_of = "eDP-1";
        };
      }
    '';
  };

  config.environment.etc."ironland-copositor/config.toml".source =
    settingsFormat.generate "ironland-copositor-config.toml" cfg.settings;
}
