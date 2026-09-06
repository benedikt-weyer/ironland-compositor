package main

import (
	"reflect"
	"testing"
)

func TestParseWaylandInfo(t *testing.T) {
	data := []byte(`interface: 'wl_output', version: 4, name: 8
	x: -1920, y: 0, scale: 1,
	make: 'Dell', model: 'U2412M',
	mode:
		width: 1920 px, height: 1200 px, refresh: 59.950 Hz,
		flags: current preferred
	mode:
		width: 1920 px, height: 1200 px, refresh: 74.997 Hz,
		flags: none
	mode:
		width: 1280 px, height: 720 px, refresh: 60.000 Hz,
		flags: none
	name: 'DP-1'
interface: 'wl_seat', version: 8, name: 9
`)
	got := parseWaylandInfo(data)
	want := []DetectedOutput{{
		Name: "DP-1", Make: "Dell", Model: "U2412M",
		X: -1920, Y: 0, Width: 1920, Height: 1200, Scale: 1,
		CurrentRefresh: 59_950, RefreshRates: []int{59_950, 74_997},
	}}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("parseWaylandInfo() = %#v, want %#v", got, want)
	}
}

func TestFormatRefreshRate(t *testing.T) {
	if got := formatRefreshRate(60_000); got != "60 Hz" {
		t.Fatalf("formatRefreshRate(60000) = %q", got)
	}
	if got := formatRefreshRate(59_950); got != "59.950 Hz" {
		t.Fatalf("formatRefreshRate(59950) = %q", got)
	}
}
