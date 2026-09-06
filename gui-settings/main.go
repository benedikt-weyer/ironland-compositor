// Command gui-settings is a small Fyne GUI for ironland-copositor's TOML
// settings file (see src/config.rs in the main crate for the schema this
// mirrors). It edits the user's own config file at
// $XDG_CONFIG_HOME/ironland-copositor/config.toml (falling back to
// ~/.config/...), which the compositor reads at startup ahead of the
// system-wide file a NixOS module may have written to /etc. Changes need a
// compositor restart to take effect.
package main

import (
	"fmt"
	"strings"

	"fyne.io/fyne/v2"
	"fyne.io/fyne/v2/app"
	"fyne.io/fyne/v2/container"
	"fyne.io/fyne/v2/dialog"
	"fyne.io/fyne/v2/storage"
	"fyne.io/fyne/v2/widget"
)

func main() {
	a := app.NewWithID("dev.ironland.copositor-settings")
	w := a.NewWindow("ironland-copositor Settings")

	cfg, loadedFrom := loadConfig()

	keyboardTab := buildKeyboardTab(&cfg)
	shortcutsTab := buildShortcutsTab(&cfg)
	outputsTab := buildOutputsTab(&cfg, w)
	workspacesTab := buildWorkspacesTab(&cfg)
	appearanceTab := buildAppearanceTab(&cfg, w)

	status := widget.NewLabel(statusText(loadedFrom))
	status.Wrapping = fyne.TextWrapWord

	saveButton := widget.NewButtonWithIcon("Save", nil, func() {
		path, err := saveConfig(cfg)
		if err != nil {
			dialog.ShowError(fmt.Errorf("saving settings: %w", err), w)
			return
		}
		status.SetText(fmt.Sprintf("Saved to %s. Restart the compositor to apply.", path))
	})
	saveButton.Importance = widget.HighImportance

	tabs := container.NewAppTabs(
		container.NewTabItem("Keyboard", keyboardTab),
		container.NewTabItem("Shortcuts", shortcutsTab),
		container.NewTabItem("Monitors", outputsTab),
		container.NewTabItem("Workspaces", workspacesTab),
		container.NewTabItem("Appearance", appearanceTab),
	)

	content := container.NewBorder(nil, container.NewVBox(status, saveButton), nil, nil, tabs)
	w.SetContent(content)
	w.Resize(fyne.NewSize(640, 520))
	w.ShowAndRun()
}

func statusText(loadedFrom string) string {
	if loadedFrom == "" {
		return "No config file found yet; showing built-in defaults."
	}
	return fmt.Sprintf("Loaded from %s", loadedFrom)
}

// buildKeyboardTab lays out entries for the keyboard layout fields and the
// terminal command, writing every edit straight back into cfg so Save
// always has the current values.
func buildKeyboardTab(cfg *Config) fyne.CanvasObject {
	layout := widget.NewEntry()
	layout.SetText(cfg.Keyboard.Layout)
	layout.SetPlaceHolder("e.g. us, de, gb (empty = system default)")
	layout.OnChanged = func(s string) { cfg.Keyboard.Layout = s }

	variant := widget.NewEntry()
	variant.SetText(cfg.Keyboard.Variant)
	variant.SetPlaceHolder("e.g. nodeadkeys, dvorak (optional)")
	variant.OnChanged = func(s string) { cfg.Keyboard.Variant = s }

	model := widget.NewEntry()
	model.SetText(cfg.Keyboard.Model)
	model.SetPlaceHolder("e.g. pc105 (optional)")
	model.OnChanged = func(s string) { cfg.Keyboard.Model = s }

	options := widget.NewEntry()
	options.SetText(cfg.Keyboard.Options)
	options.SetPlaceHolder("e.g. caps:swapescape (optional)")
	options.OnChanged = func(s string) { cfg.Keyboard.Options = s }

	rules := widget.NewEntry()
	rules.SetText(cfg.Keyboard.Rules)
	rules.SetPlaceHolder("advanced: xkb rules file (usually leave empty)")
	rules.OnChanged = func(s string) { cfg.Keyboard.Rules = s }

	terminal := widget.NewEntry()
	terminal.SetText(cfg.Terminal)
	terminal.SetPlaceHolder("command run by the \"Open terminal\" shortcut")
	terminal.OnChanged = func(s string) { cfg.Terminal = s }

	browser := widget.NewEntry()
	browser.SetText(cfg.Browser)
	browser.SetPlaceHolder("command run by the \"Open browser\" shortcut")
	browser.OnChanged = func(s string) { cfg.Browser = s }

	fileManager := widget.NewEntry()
	fileManager.SetText(cfg.FileManager)
	fileManager.SetPlaceHolder("command run by the \"Open file manager\" shortcut")
	fileManager.OnChanged = func(s string) { cfg.FileManager = s }

	topBar := widget.NewCheck("", func(checked bool) { cfg.TopBar = checked })
	topBar.SetChecked(cfg.TopBar)

	form := widget.NewForm(
		widget.NewFormItem("Layout", layout),
		widget.NewFormItem("Variant", variant),
		widget.NewFormItem("Model", model),
		widget.NewFormItem("Options", options),
		widget.NewFormItem("Rules", rules),
		widget.NewFormItem("Terminal command", terminal),
		widget.NewFormItem("Browser command", browser),
		widget.NewFormItem("File manager command", fileManager),
		widget.NewFormItem("Show top bar", topBar),
	)

	hint := widget.NewLabel("Keyboard fields are passed straight to xkbcommon; leave a field empty to use its system default. The top bar is the compositor-drawn window header/title bar; it's off by default, so windows get no server-side decoration no matter what they request.")
	hint.Wrapping = fyne.TextWrapWord

	return container.NewVBox(form, hint)
}

// buildShortcutsTab lays out one editable row per known action, each
// holding its bound key combos as a comma-separated list (e.g.
// "super+left, super+kp_left").
func buildShortcutsTab(cfg *Config) fyne.CanvasObject {
	form := widget.NewForm()

	for _, action := range knownActions {
		action := action // capture for the closure below
		entry := widget.NewEntry()
		entry.SetText(strings.Join(cfg.Shortcuts[action], ", "))
		entry.SetPlaceHolder("e.g. super+shift+q")
		entry.OnChanged = func(s string) {
			cfg.Shortcuts[action] = splitKeyCombos(s)
		}

		label := actionLabels[action]
		if label == "" {
			label = action
		}
		form.Append(label, entry)
	}

	hint := widget.NewLabel("Separate multiple key combos for the same action with commas. A combo is modifiers and a key joined with '+', e.g. \"super+alt+left\".")
	hint.Wrapping = fyne.TextWrapWord

	return container.NewBorder(nil, hint, nil, nil, container.NewVScroll(container.NewVBox(form)))
}

// buildAppearanceTab holds the dark/light mode toggle. Unlike the other
// tabs, flipping it takes effect immediately (via setSystemColorScheme)
// rather than waiting for Save, since it's a live system-wide preference and
// not just something the compositor reads back at its next start.
func buildAppearanceTab(cfg *Config, w fyne.Window) fyne.CanvasObject {
	darkMode := widget.NewCheck("Dark mode", func(checked bool) {
		cfg.Appearance.DarkMode = checked
		if err := setSystemColorScheme(checked); err != nil {
			dialog.ShowError(fmt.Errorf("setting system color scheme: %w", err), w)
		}
	})
	darkMode.SetChecked(cfg.Appearance.DarkMode)

	colorHint := widget.NewLabel(
		"Applies immediately (and is also saved to config.toml so this toggle remembers its state)." +
			" This sets the same GNOME setting that xdg-desktop-portal-gtk exposes as the org.freedesktop.appearance color scheme," +
			" so portal-aware apps pick it up too; it has no effect without xdg-desktop-portal-gtk (or gsettings) installed.",
	)
	colorHint.Wrapping = fyne.TextWrapWord

	wallpaper := widget.NewEntry()
	wallpaper.SetText(cfg.Wallpaper)
	wallpaper.SetPlaceHolder("empty = built-in default wallpaper")
	wallpaper.OnChanged = func(s string) { cfg.Wallpaper = s }

	browseButton := widget.NewButton("Browse…", func() {
		picker := dialog.NewFileOpen(func(reader fyne.URIReadCloser, err error) {
			if err != nil || reader == nil {
				return
			}
			defer reader.Close()
			path := reader.URI().Path()
			wallpaper.SetText(path)
			cfg.Wallpaper = path
		}, w)
		picker.SetFilter(storage.NewExtensionFileFilter([]string{".png", ".jpg", ".jpeg", ".webp"}))
		picker.Show()
	})

	wallpaperHint := widget.NewLabel(
		"Path to an image file (PNG/JPEG/WebP) used as the desktop background, scaled and center-cropped to" +
			" cover each output. Needs a compositor restart to take effect.",
	)
	wallpaperHint.Wrapping = fyne.TextWrapWord

	wallpaperForm := widget.NewForm(widget.NewFormItem("Wallpaper", container.NewBorder(nil, nil, nil, browseButton, wallpaper)))

	return container.NewVBox(darkMode, colorHint, widget.NewSeparator(), wallpaperForm, wallpaperHint)
}

// buildWorkspacesTab lays out the virtual-desktop settings: split-per-monitor
// vs. combined-across-monitors, the starting workspace count, whether that
// count grows/shrinks on demand, and the on-screen dot overlay toggle.
func buildWorkspacesTab(cfg *Config) fyne.CanvasObject {
	if cfg.Workspaces.Mode == "" {
		cfg.Workspaces = defaultWorkspaceSettings()
	}

	modeLabels := map[string]string{
		"per_monitor": "Split per monitor (each screen has its own workspaces)",
		"combined":    "Combined across monitors (every screen shows the same workspace)",
	}
	modeSelect := widget.NewSelect(
		[]string{modeLabels["per_monitor"], modeLabels["combined"]},
		func(selected string) {
			if selected == modeLabels["combined"] {
				cfg.Workspaces.Mode = "combined"
			} else {
				cfg.Workspaces.Mode = "per_monitor"
			}
		},
	)
	modeSelect.SetSelected(modeLabels[cfg.Workspaces.Mode])

	count := widget.NewEntry()
	count.SetText(fmt.Sprintf("%d", cfg.Workspaces.Count))
	count.OnChanged = func(s string) {
		var n int
		if _, err := fmt.Sscanf(s, "%d", &n); err == nil && n > 0 {
			cfg.Workspaces.Count = n
		}
	}

	dynamic := widget.NewCheck("Grow/shrink workspace count automatically", func(checked bool) {
		cfg.Workspaces.Dynamic = checked
	})
	dynamic.SetChecked(cfg.Workspaces.Dynamic)

	overlay := widget.NewCheck("Show workspace indicator dots on switch", func(checked bool) {
		cfg.Workspaces.Overlay = checked
	})
	overlay.SetChecked(cfg.Workspaces.Overlay)

	form := widget.NewForm(
		widget.NewFormItem("Layout", modeSelect),
		widget.NewFormItem("Starting workspace count", count),
		widget.NewFormItem("Dynamic count", dynamic),
		widget.NewFormItem("On-screen overlay", overlay),
	)

	hint := widget.NewLabel(
		"Switch workspaces with Super+Left/Right, and move the focused window to an adjacent workspace" +
			" with Super+Alt+Left/Right (both rebindable in the Shortcuts tab). With \"Dynamic count\" on," +
			" navigating or moving a window past the last workspace creates a new one, and empty trailing" +
			" workspaces are dropped again automatically; otherwise the count above is fixed.",
	)
	hint.Wrapping = fyne.TextWrapWord

	return container.NewVBox(form, hint)
}

func splitKeyCombos(s string) []string {
	var combos []string
	for _, part := range strings.Split(s, ",") {
		part = strings.TrimSpace(part)
		if part != "" {
			combos = append(combos, part)
		}
	}
	return combos
}
