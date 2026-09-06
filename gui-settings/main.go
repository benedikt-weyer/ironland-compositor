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
	w.SetFixedSize(false)
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
	defaults := defaultConfig()
	resets := newResetGroup()

	layout := widget.NewEntry()
	layout.SetText(cfg.Keyboard.Layout)
	layout.SetPlaceHolder("e.g. us, de, gb (empty = system default)")
	layout.OnChanged = func(s string) { cfg.Keyboard.Layout = s; resets.refresh() }

	variant := widget.NewEntry()
	variant.SetText(cfg.Keyboard.Variant)
	variant.SetPlaceHolder("e.g. nodeadkeys, dvorak (optional)")
	variant.OnChanged = func(s string) { cfg.Keyboard.Variant = s; resets.refresh() }

	model := widget.NewEntry()
	model.SetText(cfg.Keyboard.Model)
	model.SetPlaceHolder("e.g. pc105 (optional)")
	model.OnChanged = func(s string) { cfg.Keyboard.Model = s; resets.refresh() }

	options := widget.NewEntry()
	options.SetText(cfg.Keyboard.Options)
	options.SetPlaceHolder("e.g. caps:swapescape (optional)")
	options.OnChanged = func(s string) { cfg.Keyboard.Options = s; resets.refresh() }

	rules := widget.NewEntry()
	rules.SetText(cfg.Keyboard.Rules)
	rules.SetPlaceHolder("advanced: xkb rules file (usually leave empty)")
	rules.OnChanged = func(s string) { cfg.Keyboard.Rules = s; resets.refresh() }

	terminal := widget.NewEntry()
	terminal.SetText(cfg.Terminal)
	terminal.SetPlaceHolder("command run by the \"Open terminal\" shortcut")
	terminal.OnChanged = func(s string) { cfg.Terminal = s; resets.refresh() }

	browser := widget.NewEntry()
	browser.SetText(cfg.Browser)
	browser.SetPlaceHolder("command run by the \"Open browser\" shortcut")
	browser.OnChanged = func(s string) { cfg.Browser = s; resets.refresh() }

	fileManager := widget.NewEntry()
	fileManager.SetText(cfg.FileManager)
	fileManager.SetPlaceHolder("command run by the \"Open file manager\" shortcut")
	fileManager.OnChanged = func(s string) { cfg.FileManager = s; resets.refresh() }

	topBar := widget.NewCheck("", func(checked bool) { cfg.TopBar = checked; resets.refresh() })
	topBar.SetChecked(cfg.TopBar)

	form := widget.NewForm(
		widget.NewFormItem("Layout", resets.item(layout, func() bool { return cfg.Keyboard.Layout != defaults.Keyboard.Layout }, func() { layout.SetText(defaults.Keyboard.Layout) })),
		widget.NewFormItem("Variant", resets.item(variant, func() bool { return cfg.Keyboard.Variant != defaults.Keyboard.Variant }, func() { variant.SetText(defaults.Keyboard.Variant) })),
		widget.NewFormItem("Model", resets.item(model, func() bool { return cfg.Keyboard.Model != defaults.Keyboard.Model }, func() { model.SetText(defaults.Keyboard.Model) })),
		widget.NewFormItem("Options", resets.item(options, func() bool { return cfg.Keyboard.Options != defaults.Keyboard.Options }, func() { options.SetText(defaults.Keyboard.Options) })),
		widget.NewFormItem("Rules", resets.item(rules, func() bool { return cfg.Keyboard.Rules != defaults.Keyboard.Rules }, func() { rules.SetText(defaults.Keyboard.Rules) })),
		widget.NewFormItem("Terminal command", resets.item(terminal, func() bool { return cfg.Terminal != defaults.Terminal }, func() { terminal.SetText(defaults.Terminal) })),
		widget.NewFormItem("Browser command", resets.item(browser, func() bool { return cfg.Browser != defaults.Browser }, func() { browser.SetText(defaults.Browser) })),
		widget.NewFormItem("File manager command", resets.item(fileManager, func() bool { return cfg.FileManager != defaults.FileManager }, func() { fileManager.SetText(defaults.FileManager) })),
		widget.NewFormItem("Show top bar", resets.item(topBar, func() bool { return cfg.TopBar != defaults.TopBar }, func() { topBar.SetChecked(defaults.TopBar) })),
	)

	hint := widget.NewLabel("Keyboard fields are passed straight to xkbcommon; leave a field empty to use its system default. The top bar is the compositor-drawn window header/title bar; it's off by default, so windows get no server-side decoration no matter what they request.")
	hint.Wrapping = fyne.TextWrapWord

	return resets.page(container.NewVBox(form, hint))
}

// buildShortcutsTab lays out one editable row per known action, each
// holding its bound key combos as a comma-separated list (e.g.
// "super+left, super+kp_left").
func buildShortcutsTab(cfg *Config) fyne.CanvasObject {
	defaults := defaultShortcuts()
	resets := newResetGroup()
	form := widget.NewForm()

	for _, action := range knownActions {
		action := action // capture for the closure below
		entry := widget.NewEntry()
		entry.SetText(strings.Join(cfg.Shortcuts[action], ", "))
		entry.SetPlaceHolder("e.g. super+shift+q")
		entry.OnChanged = func(s string) {
			cfg.Shortcuts[action] = splitKeyCombos(s)
			resets.refresh()
		}

		label := actionLabels[action]
		if label == "" {
			label = action
		}
		form.Append(label, resets.item(entry,
			func() bool { return !equalStrings(cfg.Shortcuts[action], defaults[action]) },
			func() { entry.SetText(strings.Join(defaults[action], ", ")) },
		))
	}

	hint := widget.NewLabel("Separate multiple key combos for the same action with commas. A combo is modifiers and a key joined with '+', e.g. \"super+alt+left\".")
	hint.Wrapping = fyne.TextWrapWord

	return resets.page(container.NewVBox(form, hint))
}

// buildAppearanceTab holds the dark/light mode toggle. Unlike the other
// tabs, flipping it takes effect immediately (via setSystemColorScheme)
// rather than waiting for Save, since it's a live system-wide preference and
// not just something the compositor reads back at its next start.
func buildAppearanceTab(cfg *Config, w fyne.Window) fyne.CanvasObject {
	defaults := defaultConfig()
	resets := newResetGroup()
	darkMode := widget.NewCheck("Dark mode", func(checked bool) {
		cfg.Appearance.DarkMode = checked
		resets.refresh()
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
	wallpaper.OnChanged = func(s string) { cfg.Wallpaper = s; resets.refresh() }

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

	darkModeRow := resets.item(darkMode,
		func() bool { return cfg.Appearance.DarkMode != defaults.Appearance.DarkMode },
		func() { darkMode.SetChecked(defaults.Appearance.DarkMode) },
	)
	wallpaperControl := container.NewBorder(nil, nil, nil, browseButton, wallpaper)
	wallpaperForm := widget.NewForm(widget.NewFormItem("Wallpaper", resets.item(wallpaperControl,
		func() bool { return cfg.Wallpaper != defaults.Wallpaper },
		func() { wallpaper.SetText(defaults.Wallpaper) },
	)))

	return resets.page(container.NewVBox(darkModeRow, colorHint, widget.NewSeparator(), wallpaperForm, wallpaperHint))
}

// buildWorkspacesTab lays out the virtual-desktop settings: split-per-monitor
// vs. combined-across-monitors, the starting workspace count, whether that
// count grows/shrinks on demand, and the on-screen dot overlay toggle.
func buildWorkspacesTab(cfg *Config) fyne.CanvasObject {
	if cfg.Workspaces.Mode == "" {
		cfg.Workspaces = defaultWorkspaceSettings()
	}
	defaults := defaultWorkspaceSettings()
	resets := newResetGroup()

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
			resets.refresh()
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
		resets.refresh()
	}

	dynamic := widget.NewCheck("Grow/shrink workspace count automatically", func(checked bool) {
		cfg.Workspaces.Dynamic = checked
		resets.refresh()
	})
	dynamic.SetChecked(cfg.Workspaces.Dynamic)

	overlay := widget.NewCheck("Show workspace indicator dots on switch", func(checked bool) {
		cfg.Workspaces.Overlay = checked
		resets.refresh()
	})
	overlay.SetChecked(cfg.Workspaces.Overlay)

	form := widget.NewForm(
		widget.NewFormItem("Layout", resets.item(modeSelect, func() bool { return cfg.Workspaces.Mode != defaults.Mode }, func() { modeSelect.SetSelected(modeLabels[defaults.Mode]) })),
		widget.NewFormItem("Starting workspace count", resets.item(count, func() bool { return cfg.Workspaces.Count != defaults.Count }, func() { count.SetText(fmt.Sprintf("%d", defaults.Count)) })),
		widget.NewFormItem("Dynamic count", resets.item(dynamic, func() bool { return cfg.Workspaces.Dynamic != defaults.Dynamic }, func() { dynamic.SetChecked(defaults.Dynamic) })),
		widget.NewFormItem("On-screen overlay", resets.item(overlay, func() bool { return cfg.Workspaces.Overlay != defaults.Overlay }, func() { overlay.SetChecked(defaults.Overlay) })),
	)

	hint := widget.NewLabel(
		"Switch workspaces with Super+Left/Right, and move the focused window to an adjacent workspace" +
			" with Super+Alt+Left/Right (both rebindable in the Shortcuts tab). With \"Dynamic count\" on," +
			" navigating or moving a window past the last workspace creates a new one, and empty trailing" +
			" workspaces are dropped again automatically; otherwise the count above is fixed.",
	)
	hint.Wrapping = fyne.TextWrapWord

	return resets.page(container.NewVBox(form, hint))
}

func equalStrings(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
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
