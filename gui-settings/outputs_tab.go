package main

import (
	"sort"
	"strconv"

	"fyne.io/fyne/v2"
	"fyne.io/fyne/v2/container"
	"fyne.io/fyne/v2/dialog"
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
	detectionStatus := widget.NewLabel("Detecting connected monitors…")
	detectionStatus.Wrapping = fyne.TextWrapWord
	detected := map[string]DetectedOutput{}

	var rebuild func()
	refreshPageReset := func() {
		if len(cfg.Outputs) == 0 {
			resetPage.Hide()
		} else {
			resetPage.Show()
		}
	}
	resetPage.OnTapped = func() {
		cfg.Outputs = map[string]OutputSettings{}
		rebuild()
	}
	rebuild = func() {
		holder.RemoveAll()

		names := make([]string, 0, len(cfg.Outputs)+len(detected))
		seen := map[string]bool{}
		for name := range cfg.Outputs {
			names = append(names, name)
			seen[name] = true
		}
		for name := range detected {
			if !seen[name] {
				names = append(names, name)
			}
		}
		sort.Strings(names)

		if len(detected) > 0 {
			outputs := make([]DetectedOutput, 0, len(detected))
			for _, output := range detected {
				outputs = append(outputs, output)
			}
			sort.Slice(outputs, func(i, j int) bool { return outputs[i].Name < outputs[j].Name })
			holder.Add(widget.NewLabelWithStyle("Display arrangement", fyne.TextAlignLeading, fyne.TextStyle{Bold: true}))
			holder.Add(newMonitorDiagram(outputs, cfg, rebuild))
			holder.Add(widget.NewLabel("Drag displays to arrange them. Edges and corners snap together; dropping a display stores an exact logical position."))
			holder.Add(widget.NewSeparator())
		}

		for _, name := range names {
			var output *DetectedOutput
			if value, ok := detected[name]; ok {
				copy := value
				output = &copy
			}
			holder.Add(buildOutputCard(cfg, name, output, rebuild, refreshPageReset))
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
		refreshPageReset()
		holder.Refresh()
	}
	rebuild()

	detectButton := widget.NewButtonWithIcon("Detect monitors", theme.ViewRefreshIcon(), nil)
	var runDetection func()
	runDetection = func() {
		detectButton.Disable()
		detectionStatus.SetText("Detecting connected monitors…")
		go func() {
			outputs, err := detectOutputs()
			fyne.Do(func() {
				detectButton.Enable()
				if err != nil {
					detectionStatus.SetText("Automatic detection unavailable: " + err.Error() + ". You can still add a connector manually.")
					return
				}
				detected = make(map[string]DetectedOutput, len(outputs))
				for _, output := range outputs {
					detected[output.Name] = output
				}
				detectionStatus.SetText(strconv.Itoa(len(outputs)) + " connected monitor(s) detected.")
				rebuild()
			})
		}()
	}
	detectButton.OnTapped = runDetection
	runDetection()

	hint := widget.NewLabel("Connected monitors are detected through the compositor's Wayland output information. Each monitor without a saved position is auto-placed to the right of the others.")
	hint.Wrapping = fyne.TextWrapWord

	header := container.NewBorder(nil, nil, detectionStatus, container.NewHBox(detectButton, resetPage))
	body := container.NewVBox(holder, hint)
	return container.NewBorder(header, nil, nil, nil, container.NewScroll(body))
}

func buildOutputCard(cfg *Config, name string, detected *DetectedOutput, rebuild, changed func()) fyne.CanvasObject {
	settings := cfg.Outputs[name]

	titleText := name
	if detected != nil {
		titleText = monitorDescription(*detected)
	}
	title := widget.NewLabelWithStyle(titleText, fyne.TextAlignLeading, fyne.TextStyle{Bold: true})

	removeButton := widget.NewButtonWithIcon("Reset monitor", theme.ViewRefreshIcon(), func() {
		delete(cfg.Outputs, name)
		rebuild()
	})
	refreshMonitorReset := func() {
		if _, configured := cfg.Outputs[name]; configured {
			removeButton.Show()
		} else {
			removeButton.Hide()
		}
		changed()
	}
	refreshMonitorReset()

	primaryReset := widget.NewButtonWithIcon("Reset", theme.ViewRefreshIcon(), nil)
	var refreshPrimaryReset func()
	primary := widget.NewCheck("Primary monitor", nil)
	primary.SetChecked(settings.Primary)
	primary.OnChanged = func(checked bool) {
		s := cfg.Outputs[name]
		s.Primary = checked
		storeOutputSettings(cfg, name, s)
		refreshPrimaryReset()
		refreshMonitorReset()
	}
	refreshPrimaryReset = func() {
		if cfg.Outputs[name].Primary {
			primaryReset.Show()
		} else {
			primaryReset.Hide()
		}
	}
	primaryReset.OnTapped = func() { primary.SetChecked(false) }
	refreshPrimaryReset()

	refreshReset := widget.NewButtonWithIcon("Reset", theme.ViewRefreshIcon(), nil)
	refreshOptions := []string{"Automatic"}
	refreshValues := map[string]int{"Automatic": 0}
	if detected != nil {
		for _, rate := range detected.RefreshRates {
			label := formatRefreshRate(rate)
			refreshOptions = append(refreshOptions, label)
			refreshValues[label] = rate
		}
	}
	refreshSelect := widget.NewSelect(refreshOptions, nil)
	selectedRefresh := "Automatic"
	if settings.RefreshRate != 0 {
		selectedRefresh = formatRefreshRate(settings.RefreshRate)
		if _, ok := refreshValues[selectedRefresh]; !ok {
			refreshOptions = append(refreshOptions, selectedRefresh)
			refreshSelect.Options = refreshOptions
			refreshValues[selectedRefresh] = settings.RefreshRate
		}
	}
	refreshSelect.SetSelected(selectedRefresh)
	refreshRefreshReset := func() {
		if cfg.Outputs[name].RefreshRate == 0 {
			refreshReset.Hide()
		} else {
			refreshReset.Show()
		}
	}
	refreshSelect.OnChanged = func(selected string) {
		s := cfg.Outputs[name]
		s.RefreshRate = refreshValues[selected]
		storeOutputSettings(cfg, name, s)
		refreshRefreshReset()
		refreshMonitorReset()
	}
	refreshReset.OnTapped = func() { refreshSelect.SetSelected("Automatic") }
	refreshRefreshReset()

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
	setExtraRows := func(selected string) {
		extraRows.RemoveAll()
		switch positionMode(selected) {
		case modeAuto:
		case modeRightOf, modeLeftOf, modeAbove, modeBelow:
			extraRows.Add(targetRow)
		case modeAbsolute:
			extraRows.Add(xyRow)
		case modeMirror:
			extraRows.Add(mirrorRow)
		}
		extraRows.Refresh()
	}
	mode.SetSelected(string(modeOf(settings)))
	setExtraRows(mode.Selected)
	mode.OnChanged = func(selected string) {
		s := cfg.Outputs[name]
		storeOutputSettings(cfg, name, applyModeField(s, positionMode(selected), targetEntry.Text, xEntry.Text, yEntry.Text))
		setExtraRows(selected)
		refreshPositionReset()
		refreshMonitorReset()
	}

	targetEntry.OnChanged = func(text string) {
		s := cfg.Outputs[name]
		storeOutputSettings(cfg, name, applyModeField(s, positionMode(mode.Selected), text, xEntry.Text, yEntry.Text))
		refreshPositionReset()
		refreshMonitorReset()
	}
	xEntry.OnChanged = func(text string) {
		s := cfg.Outputs[name]
		storeOutputSettings(cfg, name, applyModeField(s, positionMode(mode.Selected), targetEntry.Text, text, yEntry.Text))
		refreshPositionReset()
		refreshMonitorReset()
	}
	yEntry.OnChanged = func(text string) {
		s := cfg.Outputs[name]
		storeOutputSettings(cfg, name, applyModeField(s, positionMode(mode.Selected), targetEntry.Text, xEntry.Text, text))
		refreshPositionReset()
		refreshMonitorReset()
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
	refreshRow := container.NewBorder(nil, nil, widget.NewLabel("Refresh rate:"), refreshReset, refreshSelect)
	positionRow := container.NewBorder(nil, nil, nil, positionReset, mode)
	body := container.NewVBox(primaryRow, refreshRow, positionRow, extraRows)

	return container.NewVBox(header, body, widget.NewSeparator())
}

func storeOutputSettings(cfg *Config, name string, settings OutputSettings) {
	if settings == (OutputSettings{}) {
		delete(cfg.Outputs, name)
		return
	}
	cfg.Outputs[name] = settings
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
