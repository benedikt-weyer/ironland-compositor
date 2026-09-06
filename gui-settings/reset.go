package main

import (
	"fyne.io/fyne/v2"
	"fyne.io/fyne/v2/container"
	"fyne.io/fyne/v2/layout"
	"fyne.io/fyne/v2/theme"
	"fyne.io/fyne/v2/widget"
)

// resetGroup keeps the reset affordances for one settings page in sync.
// Call refresh after changing a setting; buttons are visible only while the
// corresponding value differs from the built-in default.
type resetGroup struct {
	items      []resetItem
	pageButton *widget.Button
}

type resetItem struct {
	button    *widget.Button
	different func() bool
	reset     func()
}

func newResetGroup() *resetGroup {
	g := &resetGroup{}
	g.pageButton = widget.NewButtonWithIcon("Reset page", theme.ViewRefreshIcon(), func() {
		for _, item := range g.items {
			if item.different() {
				item.reset()
			}
		}
		g.refresh()
	})
	g.pageButton.Hide()
	return g
}

func (g *resetGroup) item(control fyne.CanvasObject, different func() bool, reset func()) fyne.CanvasObject {
	button := widget.NewButtonWithIcon("Reset", theme.ViewRefreshIcon(), func() {
		reset()
		g.refresh()
	})
	button.Hide()
	g.items = append(g.items, resetItem{button: button, different: different, reset: reset})
	return container.NewBorder(nil, nil, nil, button, control)
}

func (g *resetGroup) refresh() {
	pageDiffers := false
	for _, item := range g.items {
		if item.different() {
			item.button.Show()
			pageDiffers = true
		} else {
			item.button.Hide()
		}
	}
	if pageDiffers {
		g.pageButton.Show()
	} else {
		g.pageButton.Hide()
	}
}

// page adds a page-level reset and two-axis scrolling. The scroll container
// lets the settings window be resized freely in either direction instead of
// inheriting a fixed minimum width/height from the largest form.
func (g *resetGroup) page(body fyne.CanvasObject) fyne.CanvasObject {
	g.refresh()
	header := container.NewHBox(layout.NewSpacer(), g.pageButton)
	return container.NewBorder(header, nil, nil, nil, container.NewScroll(body))
}
