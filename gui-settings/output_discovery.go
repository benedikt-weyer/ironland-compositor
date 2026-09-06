package main

import (
	"bufio"
	"bytes"
	"context"
	"fmt"
	"os/exec"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"time"
)

// DetectedOutput is the subset of wl_output state used by the settings UI.
// Refresh rates are stored in millihertz, matching the Wayland protocol and
// the compositor config.
type DetectedOutput struct {
	Name, Make, Model          string
	X, Y, Width, Height, Scale int
	CurrentRefresh             int
	RefreshRates               []int
}

type detectedMode struct {
	width, height, refresh int
	current, preferred     bool
}

var (
	outputHeaderRE = regexp.MustCompile(`^interface: '([^']+)'`)
	geometryRE     = regexp.MustCompile(`x:\s*(-?\d+),\s*y:\s*(-?\d+),\s*scale:\s*(\d+)`)
	makeModelRE    = regexp.MustCompile(`make:\s*'([^']*)',\s*model:\s*'([^']*)'`)
	outputNameRE   = regexp.MustCompile(`^\s+name:\s*'([^']+)'`)
	modeRE         = regexp.MustCompile(`width:\s*(\d+)\s*px,\s*height:\s*(\d+)\s*px,\s*refresh:\s*([0-9.]+)\s*Hz`)
)

func detectOutputs() ([]DetectedOutput, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 4*time.Second)
	defer cancel()
	out, err := exec.CommandContext(ctx, "wayland-info").Output()
	if err != nil {
		return nil, fmt.Errorf("running wayland-info: %w", err)
	}
	outputs := parseWaylandInfo(out)
	if len(outputs) == 0 {
		return nil, fmt.Errorf("wayland-info reported no monitors")
	}
	return outputs, nil
}

func parseWaylandInfo(data []byte) []DetectedOutput {
	var outputs []DetectedOutput
	var current *DetectedOutput
	var modes []detectedMode

	finish := func() {
		if current == nil || current.Name == "" {
			current, modes = nil, nil
			return
		}
		if current.Scale < 1 {
			current.Scale = 1
		}
		chosen := -1
		for i, mode := range modes {
			if mode.current || (chosen < 0 && mode.preferred) {
				chosen = i
				if mode.current {
					break
				}
			}
		}
		if chosen < 0 && len(modes) > 0 {
			chosen = 0
		}
		if chosen >= 0 {
			mode := modes[chosen]
			current.Width = mode.width / current.Scale
			current.Height = mode.height / current.Scale
			current.CurrentRefresh = mode.refresh
			seen := map[int]bool{}
			for _, candidate := range modes {
				if candidate.width == mode.width && candidate.height == mode.height && !seen[candidate.refresh] {
					seen[candidate.refresh] = true
					current.RefreshRates = append(current.RefreshRates, candidate.refresh)
				}
			}
			sort.Ints(current.RefreshRates)
		}
		if current.Width <= 0 {
			current.Width, current.Height = 1920, 1080
		}
		outputs = append(outputs, *current)
		current, modes = nil, nil
	}

	scanner := bufio.NewScanner(bytes.NewReader(data))
	for scanner.Scan() {
		line := scanner.Text()
		if match := outputHeaderRE.FindStringSubmatch(line); match != nil {
			finish()
			if match[1] == "wl_output" {
				current = &DetectedOutput{Scale: 1}
			}
			continue
		}
		if current == nil {
			continue
		}
		if match := geometryRE.FindStringSubmatch(line); match != nil {
			current.X, _ = strconv.Atoi(match[1])
			current.Y, _ = strconv.Atoi(match[2])
			current.Scale, _ = strconv.Atoi(match[3])
		} else if match := makeModelRE.FindStringSubmatch(line); match != nil {
			current.Make, current.Model = match[1], match[2]
		} else if match := outputNameRE.FindStringSubmatch(line); match != nil {
			current.Name = match[1]
		} else if match := modeRE.FindStringSubmatch(line); match != nil {
			width, _ := strconv.Atoi(match[1])
			height, _ := strconv.Atoi(match[2])
			hz, _ := strconv.ParseFloat(match[3], 64)
			modes = append(modes, detectedMode{width: width, height: height, refresh: int(hz*1000 + 0.5)})
		} else if strings.Contains(line, "flags:") && len(modes) > 0 {
			last := &modes[len(modes)-1]
			last.current = strings.Contains(line, "current")
			last.preferred = strings.Contains(line, "preferred")
		}
	}
	finish()
	sort.Slice(outputs, func(i, j int) bool { return outputs[i].Name < outputs[j].Name })
	return outputs
}

func formatRefreshRate(rate int) string {
	if rate%1000 == 0 {
		return fmt.Sprintf("%d Hz", rate/1000)
	}
	return fmt.Sprintf("%.3f Hz", float64(rate)/1000)
}
