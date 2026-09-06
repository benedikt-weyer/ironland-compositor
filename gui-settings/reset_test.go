package main

import (
	"testing"

	"fyne.io/fyne/v2/test"
	"fyne.io/fyne/v2/widget"
)

func TestResetGroupVisibilityAndActions(t *testing.T) {
	app := test.NewApp()
	t.Cleanup(app.Quit)

	value := "default"
	g := newResetGroup()
	g.item(widget.NewEntry(), func() bool { return value != "default" }, func() { value = "default" })

	g.refresh()
	if g.items[0].button.Visible() || g.pageButton.Visible() {
		t.Fatal("reset buttons should be hidden for a default value")
	}

	value = "custom"
	g.refresh()
	if !g.items[0].button.Visible() || !g.pageButton.Visible() {
		t.Fatal("item and page reset buttons should be visible for a custom value")
	}

	g.items[0].button.OnTapped()
	if value != "default" || g.items[0].button.Visible() || g.pageButton.Visible() {
		t.Fatal("item reset should restore the default and hide both buttons")
	}

	value = "custom"
	g.refresh()
	g.pageButton.OnTapped()
	if value != "default" || g.items[0].button.Visible() || g.pageButton.Visible() {
		t.Fatal("page reset should restore the default and hide both buttons")
	}
}
