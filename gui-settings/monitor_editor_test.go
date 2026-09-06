package main

import "testing"

func TestEffectiveMonitorRectsUsesSavedPositions(t *testing.T) {
	outputs := []DetectedOutput{
		{Name: "DP-1", X: 0, Y: 0, Width: 1920, Height: 1080},
		{Name: "HDMI-A-1", X: 1920, Y: 0, Width: 1280, Height: 720},
	}
	settings := map[string]OutputSettings{
		"DP-1":     {Position: &OutputPosition{X: intPointer(-1920), Y: intPointer(100)}},
		"HDMI-A-1": {Position: &OutputPosition{Below: "DP-1"}},
	}
	rects := effectiveMonitorRects(outputs, settings)
	if got := rects["DP-1"]; got.x != -1920 || got.y != 100 {
		t.Fatalf("DP-1 position = (%d,%d), want (-1920,100)", got.x, got.y)
	}
	if got := rects["HDMI-A-1"]; got.x != -1920 || got.y != 1180 {
		t.Fatalf("HDMI-A-1 position = (%d,%d), want (-1920,1180)", got.x, got.y)
	}
}
