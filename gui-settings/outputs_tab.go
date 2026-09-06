package main

import (
	"sort"
	"strconv"

	"fyne.io/fyne/v2"
	"fyne.io/fyne/v2/container"
	"fyne.io/fyne/v2/dialog"
	"fyne.io/fyne/v2/layout"
	"fyne.io/fyne/v2/theme"
	"fyne.io/fyne/v2/widget"
)

// positionMode is the GUI-level equivalent of the possible shapes of
// *OutputPosition plus "no position set" and "mirroring", collapsed into one
// selector per monitor.
type positionMode string

const (
	modeAuto     positionMode = "Extend (auto-placed)"
	modeRightOf  positionMode = "Extend: right of..."
	modeLeftOf   positionMode = "Extend: left of..."
	modeAbove    positionMode = "Extend: above..."
	modeBelow    positionMode = "Extend: below..."
	modeAbsolute positionMode = "Extend: at position..."
	modeMirror   positionMode = "Duplicate (mirror)..."
)

var positionModes = []string{
	string(modeAuto),
	string(modeRightOf),
	string(modeLeftOf),
	string(modeAbove),
	string(modeBelow),
	string(modeAbsolute),
	string(modeMirror),
}

// modeOf classifies an OutputSettings' current configuration into the
// selector's mode, so re-opening the tab shows what's actually in cfg.
func modeOf(s OutputSettings) positionMode {
	if s.MirrorOf != "" {
		return modeMirror
	}
	if s.Position == nil {
		return modeAuto
	}
	switch {
	case s.Position.RightOf != "":
		return modeRightOf
	case s.Position.LeftOf != "":
		return modeLeftOf
	case s.Position.Above != "":
		return modeAbove
	case s.Position.Below != "":
		return modeBelow
	default:
		return modeAbsolute
	}
}

// buildOutputsTab lays out one card per configured monitor (matched by
// connector name, e.g. "eDP-1", "HDMI-A-1") plus a way to add more. Every
// edit writes straight back into cfg.Outputs so Save always has the
// current values; `rebuild` re-renders the whole tab, which is the
// simplest way to keep the list-of-cards in sync after an add/remove.
func buildOutputsTab(cfg *Config, w fyne.Window) fyne.CanvasObject {
	holder := container.NewVBox()
	resetPage := widget.NewButtonWithIcon("Reset page", theme.ViewRefreshIcon(), nil)
	resetPage.Hide()

	var rebuild func()
	resetPage.OnTapped = func() {
		cfg.Outputs = map[string]OutputSettings{}
		rebuild()
	}
	rebuild = func() {
		holder.RemoveAll()

		names := make([]string, 0, len(cfg.Outputs))
		for name := range cfg.Outputs {
			names = append(names, name)
		}
		sort.Strings(names)

		for _, name := range names {
			holder.Add(buildOutputCard(cfg, name, rebuild))
		}

		addName := widget.NewEntry()
		addName.SetPlaceHolder(`connector name, e.g. "eDP-1" or "HDMI-A-1"`)
		addButton := widget.NewButtonWithIcon("Add monitor", nil, func() {
			name := addName.Text
			if name == "" {
				return
			}
			if _, exists := cfg.Outputs[name]; exists {
				dialog.ShowInformation("Already added", name+" is already configured.", w)
				return
			}
			cfg.Outputs[name] = OutputSettings{}
			rebuild()
		})
		holder.Add(container.NewBorder(nil, nil, nil, addButton, addName))
		if len(cfg.Outputs) == 0 {
			resetPage.Hide()
		} else {
			resetPage.Show()
		}

		holder.Refresh()
	}
	rebuild()

	hint := widget.NewLabel("Connector names come from the compositor's logs on connect (e.g. \"Trying to setup connector eDP-1\"), or from `wlr-randr`/`kanshi` output names. Each monitor not listed here is auto-placed to the right of the others.")
	hint.Wrapping = fyne.TextWrapWord

	header := container.NewHBox(layout.NewSpacer(), resetPage)
	body := container.NewVBox(holder, hint)
	return container.NewBorder(header, nil, nil, nil, container.NewScroll(body))
}

func buildOutputCard(cfg *Config, name string, rebuild func()) fyne.CanvasObject {
	settings := cfg.Outputs[name]

	title := widget.NewLabelWithStyle(name, fyne.TextAlignLeading, fyne.TextStyle{Bold: true})

	removeButton := widget.NewButtonWithIcon("Reset monitor", theme.ViewRefreshIcon(), func() {
		delete(cfg.Outputs, name)
		rebuild()
	})

	primaryReset := widget.NewButtonWithIcon("Reset", theme.ViewRefreshIcon(), nil)
	var refreshPrimaryReset func()
	primary := widget.NewCheck("Primary monitor", func(checked bool) {
		s := cfg.Outputs[name]
		s.Primary = checked
		cfg.Outputs[name] = s
		refreshPrimaryReset()
	})
	refreshPrimaryReset = func() {
		if cfg.Outputs[name].Primary {
			primaryReset.Show()
		} else {
			primaryReset.Hide()
		}
	}
	primaryReset.OnTapped = func() { primary.SetChecked(false) }
	primary.SetChecked(settings.Primary)
	refreshPrimaryReset()

	targetEntry := widget.NewEntry()
	targetEntry.SetPlaceHolder("other monitor's connector name")

	xEntry := widget.NewEntry()
	xEntry.SetPlaceHolder("x")
	yEntry := widget.NewEntry()
	yEntry.SetPlaceHolder("y")

	// Prefill the mode-specific fields from whatever's currently set, so
	// switching modes and back doesn't lose what was there.
	switch {
	case settings.MirrorOf != "":
		targetEntry.SetText(settings.MirrorOf)
	case settings.Position != nil:
		switch {
		case settings.Position.RightOf != "":
			targetEntry.SetText(settings.Position.RightOf)
		case settings.Position.LeftOf != "":
			targetEntry.SetText(settings.Position.LeftOf)
		case settings.Position.Above != "":
			targetEntry.SetText(settings.Position.Above)
		case settings.Position.Below != "":
			targetEntry.SetText(settings.Position.Below)
		default:
			if settings.Position.X != nil {
				xEntry.SetText(strconv.Itoa(*settings.Position.X))
			}
			if settings.Position.Y != nil {
				yEntry.SetText(strconv.Itoa(*settings.Position.Y))
			}
		}
	}

	targetRow := container.NewBorder(nil, nil, widget.NewLabel("Relative to:"), nil, targetEntry)
	xyRow := container.NewGridWithColumns(2,
		container.NewBorder(nil, nil, widget.NewLabel("X:"), nil, xEntry),
		container.NewBorder(nil, nil, widget.NewLabel("Y:"), nil, yEntry),
	)
	mirrorRow := container.NewBorder(nil, nil, widget.NewLabel("Duplicate of:"), nil, targetEntry)

	extraRows := container.NewVBox()
	positionReset := widget.NewButtonWithIcon("Reset", theme.ViewRefreshIcon(), nil)
	var refreshPositionReset func()

	mode := widget.NewSelect(positionModes, nil)
	refreshPositionReset = func() {
		if modeOf(cfg.Outputs[name]) == modeAuto {
			positionReset.Hide()
		} else {
			positionReset.Show()
		}
	}
	mode.OnChanged = func(selected string) {
		s := cfg.Outputs[name]
		cfg.Outputs[name] = applyModeField(s, positionMode(selected), targetEntry.Text, xEntry.Text, yEntry.Text)

		extraRows.RemoveAll()
		switch positionMode(selected) {
		case modeAuto:
			// Nothing more to configure.
		case modeRightOf, modeLeftOf, modeAbove, modeBelow:
			extraRows.Add(targetRow)
		case modeAbsolute:
			extraRows.Add(xyRow)
		case modeMirror:
			extraRows.Add(mirrorRow)
		}
		extraRows.Refresh()
		refreshPositionReset()
	}
	mode.SetSelected(string(modeOf(settings)))

	targetEntry.OnChanged = func(text string) {
		s := cfg.Outputs[name]
		cfg.Outputs[name] = applyModeField(s, positionMode(mode.Selected), text, xEntry.Text, yEntry.Text)
		refreshPositionReset()
	}
	xEntry.OnChanged = func(text string) {
		s := cfg.Outputs[name]
		cfg.Outputs[name] = applyModeField(s, positionMode(mode.Selected), targetEntry.Text, text, yEntry.Text)
		refreshPositionReset()
	}
	yEntry.OnChanged = func(text string) {
		s := cfg.Outputs[name]
		cfg.Outputs[name] = applyModeField(s, positionMode(mode.Selected), targetEntry.Text, xEntry.Text, text)
		refreshPositionReset()
	}
	positionReset.OnTapped = func() {
		targetEntry.SetText("")
		xEntry.SetText("")
		yEntry.SetText("")
		mode.SetSelected(string(modeAuto))
		refreshPositionReset()
	}
	refreshPositionReset()

	header := container.NewBorder(nil, nil, title, removeButton)
	primaryRow := container.NewBorder(nil, nil, nil, primaryReset, primary)
	positionRow := container.NewBorder(nil, nil, nil, positionReset, mode)
	body := container.NewVBox(primaryRow, positionRow, extraRows)

	return container.NewVBox(header, body, widget.NewSeparator())
}

// applyModeField rewrites s' MirrorOf/Position from the selector's current
// mode plus the (possibly just-edited) target/x/y field text, leaving
// Primary untouched.
func applyModeField(s OutputSettings, mode positionMode, target, xText, yText string) OutputSettings {
	s.MirrorOf = ""
	s.Position = nil

	switch mode {
	case modeAuto:
	case modeMirror:
		s.MirrorOf = target
	case modeRightOf:
		s.Position = &OutputPosition{RightOf: target}
	case modeLeftOf:
		s.Position = &OutputPosition{LeftOf: target}
	case modeAbove:
		s.Position = &OutputPosition{Above: target}
	case modeBelow:
		s.Position = &OutputPosition{Below: target}
	case modeAbsolute:
		pos := &OutputPosition{}
		if x, err := strconv.Atoi(xText); err == nil {
			pos.X = &x
		}
		if y, err := strconv.Atoi(yText); err == nil {
			pos.Y = &y
		}
		s.Position = pos
	}
	return s
}
