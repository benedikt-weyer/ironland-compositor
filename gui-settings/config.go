package main

import (
	"os"
	"path/filepath"

	"github.com/BurntSushi/toml"
)

// KeyboardSettings mirrors `config::KeyboardSettings` in the compositor
// (src/config.rs): fields are passed straight through to xkbcommon, and an
// empty string means "let xkbcommon fall back to its XKB_DEFAULT_* env vars
// / built-in default".
type KeyboardSettings struct {
	Rules   string `toml:"rules"`
	Model   string `toml:"model"`
	Layout  string `toml:"layout"`
	Variant string `toml:"variant"`
	Options string `toml:"options"`
}

// AppearanceSettings is GUI-only state: the compositor itself has no notion
// of a color scheme, so this isn't mirrored in config::Config on the Rust
// side (its TOML parser ignores tables it doesn't know about). It's kept in
// the same config.toml purely so the GUI remembers the toggle across runs.
type AppearanceSettings struct {
	DarkMode bool `toml:"dark_mode"`
}

type BlurSettings struct {
	Enabled bool `toml:"enabled"`
	Radius  int  `toml:"radius"`
}

// WorkspaceSettings mirrors `config::WorkspaceSettings` in the compositor:
// how many virtual desktops exist, whether each output gets its own set or
// every output shares one, whether the count grows/shrinks on demand, and
// whether the on-screen dot indicator flashes on switch.
type WorkspaceSettings struct {
	// Mode is either "per_monitor" (each output has its own workspaces) or
	// "combined" (every output shows the same workspace at once).
	Mode    string `toml:"mode"`
	Count   int    `toml:"count"`
	Dynamic bool   `toml:"dynamic"`
	Overlay bool   `toml:"overlay"`
}

// Config mirrors `config::Config` / `config::RawConfig` in the compositor,
// plus the GUI-only Appearance settings above.
type Config struct {
	Keyboard    KeyboardSettings `toml:"keyboard"`
	Terminal    string           `toml:"terminal"`
	Browser     string           `toml:"browser"`
	FileManager string           `toml:"file_manager"`
	// TopBar controls whether windows may get a compositor-drawn header
	// bar for server-side decoration. Off by default: a client's request
	// for server-side decoration is overridden back to client-side.
	TopBar bool `toml:"top_bar"`
	// Wallpaper is a path to an image file (PNG/JPEG/WebP) used as the
	// desktop background, scaled and center-cropped to cover each output.
	// Empty uses the compositor's built-in default wallpaper.
	Wallpaper  string                    `toml:"wallpaper,omitempty"`
	Blur       BlurSettings              `toml:"blur"`
	Appearance AppearanceSettings        `toml:"appearance"`
	Shortcuts  map[string][]string       `toml:"shortcuts"`
	Outputs    map[string]OutputSettings `toml:"outputs"`
	Workspaces WorkspaceSettings         `toml:"workspaces"`
}

// OutputPosition mirrors `config::OutputPosition` in the compositor: exactly
// one of RightOf/LeftOf/Above/Below (each another output's connector name)
// or X/Y (an absolute logical position) should be set. It's kept flat
// rather than as a Go-level tagged union because that's how it round-trips
// through TOML into Rust's `#[serde(untagged)]` enum: only the keys present
// in the table matter, and `omitempty` keeps the others out of the file.
type OutputPosition struct {
	RightOf string `toml:"right_of,omitempty"`
	LeftOf  string `toml:"left_of,omitempty"`
	Above   string `toml:"above,omitempty"`
	Below   string `toml:"below,omitempty"`
	X       *int   `toml:"x,omitempty"`
	Y       *int   `toml:"y,omitempty"`
}

// OutputSettings mirrors `config::OutputSettings` in the compositor, keyed
// by connector name (e.g. "eDP-1", "HDMI-A-1") in Config.Outputs.
type OutputSettings struct {
	Primary     bool            `toml:"primary,omitempty"`
	RefreshRate int             `toml:"refresh_rate,omitempty"`
	MirrorOf    string          `toml:"mirror_of,omitempty"`
	Position    *OutputPosition `toml:"position,omitempty"`
}

// knownActions lists every action the compositor recognizes in
// [shortcuts], in the same order as `config::known_actions` in
// src/config.rs. Keep the two in sync: an action missing here just won't
// be editable in the GUI, and one missing there is silently ignored by the
// compositor with a warning.
var knownActions = []string{
	"quit",
	"run_terminal",
	"toggle_launcher",
	"open_browser",
	"open_file_manager",
	"toggle_floating",
	"kill_window",
	"focus_left",
	"focus_right",
	"focus_up",
	"focus_down",
	"swap_left",
	"swap_right",
	"swap_up",
	"swap_down",
	"resize_left",
	"resize_right",
	"resize_up",
	"resize_down",
	"workspace_left",
	"workspace_right",
	"move_workspace_left",
	"move_workspace_right",
	"scale_up",
	"scale_down",
	"toggle_preview",
	"rotate_output",
	"toggle_tint",
	"toggle_decorations",
}

// actionLabels gives each action a human-readable name for the GUI.
var actionLabels = map[string]string{
	"quit":                 "Quit compositor",
	"run_terminal":         "Open terminal",
	"toggle_launcher":      "Toggle app launcher",
	"open_browser":         "Open browser",
	"open_file_manager":    "Open file manager",
	"toggle_floating":      "Toggle floating/tiled",
	"kill_window":          "Kill active window",
	"focus_left":           "Focus window: left",
	"focus_right":          "Focus window: right",
	"focus_up":             "Focus window: up",
	"focus_down":           "Focus window: down",
	"swap_left":            "Swap window: left",
	"swap_right":           "Swap window: right",
	"swap_up":              "Swap window: up",
	"swap_down":            "Swap window: down",
	"resize_left":          "Resize tiled window: left",
	"resize_right":         "Resize tiled window: right",
	"resize_up":            "Resize tiled window: up",
	"resize_down":          "Resize tiled window: down",
	"workspace_left":       "Switch workspace: previous",
	"workspace_right":      "Switch workspace: next",
	"move_workspace_left":  "Move window to workspace: previous",
	"move_workspace_right": "Move window to workspace: next",
	"scale_up":             "Increase output scale",
	"scale_down":           "Decrease output scale",
	"toggle_preview":       "Toggle window preview",
	"rotate_output":        "Rotate output",
	"toggle_tint":          "Toggle debug tint",
	"toggle_decorations":   "Toggle window decorations",
}

// defaultShortcuts is the baseline the compositor falls back to for any
// action not overridden in the config file. Mirrors
// `config::default_shortcuts` in src/config.rs.
func defaultShortcuts() map[string][]string {
	return map[string][]string{
		"quit":                 {"super+alt+backspace", "super+q"},
		"run_terminal":         {"super+c"},
		"toggle_launcher":      {"super"},
		"open_browser":         {"super+b"},
		"open_file_manager":    {"super+f"},
		"toggle_floating":      {"super+shift+space"},
		"kill_window":          {"super+x"},
		"focus_left":           {"super+ctrl+left"},
		"focus_right":          {"super+ctrl+right"},
		"focus_up":             {"super+up"},
		"focus_down":           {"super+down"},
		"swap_left":            {"super+shift+left"},
		"swap_right":           {"super+shift+right"},
		"swap_up":              {"super+shift+up"},
		"swap_down":            {"super+shift+down"},
		"resize_left":          {"super+ctrl+shift+left"},
		"resize_right":         {"super+ctrl+shift+right"},
		"resize_up":            {"super+alt+up"},
		"resize_down":          {"super+alt+down"},
		"workspace_left":       {"super+left"},
		"workspace_right":      {"super+right"},
		"move_workspace_left":  {"super+alt+left"},
		"move_workspace_right": {"super+alt+right"},
		"scale_up":             {"super+shift+p"},
		"scale_down":           {"super+shift+m"},
		"toggle_preview":       {"super+shift+w"},
		"rotate_output":        {"super+shift+r"},
		"toggle_tint":          {"super+shift+t"},
		"toggle_decorations":   {"super+shift+d"},
	}
}

func defaultWorkspaceSettings() WorkspaceSettings {
	return WorkspaceSettings{
		Mode:    "per_monitor",
		Count:   4,
		Dynamic: false,
		Overlay: true,
	}
}

func defaultConfig() Config {
	return Config{
		Terminal:    "weston-terminal",
		Browser:     "brave",
		FileManager: "iron-file",
		Blur:        BlurSettings{Radius: 12},
		Shortcuts:   defaultShortcuts(),
		Outputs:     map[string]OutputSettings{},
		Workspaces:  defaultWorkspaceSettings(),
	}
}

// userConfigPath is where this GUI saves settings: the same
// `$XDG_CONFIG_HOME`/`~/.config` location the compositor checks before
// falling back to `/etc/ironland-copositor/config.toml`, and one a normal
// user can write without root.
func userConfigPath() (string, error) {
	configHome := os.Getenv("XDG_CONFIG_HOME")
	if configHome == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return "", err
		}
		configHome = filepath.Join(home, ".config")
	}
	return filepath.Join(configHome, "ironland-copositor", "config.toml"), nil
}

// configSearchPath mirrors `config::config_search_path` in src/config.rs:
// the same explicit-override env var, the same user config path, then the
// system-wide file the NixOS module writes.
func configSearchPath() []string {
	var paths []string
	if explicit := os.Getenv("IRONLAND_COMPOSITOR_CONFIG"); explicit != "" {
		paths = append(paths, explicit)
	}
	if userPath, err := userConfigPath(); err == nil {
		paths = append(paths, userPath)
	}
	paths = append(paths, "/etc/ironland-copositor/config.toml")
	return paths
}

// loadConfig returns the settings that would be active if the compositor
// started right now (the first config file found on configSearchPath,
// merged over the built-in defaults), plus the path it came from - or ""
// if none of the candidate files exist yet.
func loadConfig() (Config, string) {
	cfg := defaultConfig()

	for _, path := range configSearchPath() {
		data, err := os.ReadFile(path)
		if err != nil {
			continue
		}

		var raw Config
		if _, err := toml.Decode(string(data), &raw); err != nil {
			// Malformed file: same as the compositor, fall back to defaults
			// rather than erroring out.
			return defaultConfig(), ""
		}

		if raw.Terminal != "" {
			cfg.Terminal = raw.Terminal
		}
		if raw.Browser != "" {
			cfg.Browser = raw.Browser
		}
		if raw.FileManager != "" {
			cfg.FileManager = raw.FileManager
		}
		cfg.TopBar = raw.TopBar
		cfg.Wallpaper = raw.Wallpaper
		cfg.Blur.Enabled = raw.Blur.Enabled
		if raw.Blur.Radius > 0 {
			cfg.Blur.Radius = raw.Blur.Radius
		}
		cfg.Appearance = raw.Appearance
		cfg.Keyboard = raw.Keyboard
		for action, keys := range raw.Shortcuts {
			cfg.Shortcuts[action] = keys
		}
		if raw.Outputs != nil {
			cfg.Outputs = raw.Outputs
		}
		if raw.Workspaces.Mode != "" {
			cfg.Workspaces = raw.Workspaces
		}
		return cfg, path
	}

	return cfg, ""
}

// saveConfig writes cfg to the user's config file, creating its parent
// directory if needed, and returns the path it wrote to.
func saveConfig(cfg Config) (string, error) {
	path, err := userConfigPath()
	if err != nil {
		return "", err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return "", err
	}

	f, err := os.Create(path)
	if err != nil {
		return "", err
	}
	defer f.Close()

	if err := toml.NewEncoder(f).Encode(cfg); err != nil {
		return "", err
	}
	return path, nil
}
