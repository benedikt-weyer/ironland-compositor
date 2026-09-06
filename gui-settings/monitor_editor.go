package main

import (
	"fmt"
	"image/color"
	"math"

	"fyne.io/fyne/v2"
	"fyne.io/fyne/v2/canvas"
	"fyne.io/fyne/v2/container"
	"fyne.io/fyne/v2/theme"
	"fyne.io/fyne/v2/widget"
)

const (
	monitorStageWidth   float32 = 720
	monitorStageHeight  float32 = 280
	monitorStagePad     float32 = 20
	monitorSnapDistance float32 = 12
)

type logicalRect struct {
	x, y, width, height int
}

type monitorDiagram struct {
	stage    *fyne.Container
	tiles    []*monitorTile
	minX     int
	minY     int
	scale    float32
	cfg      *Config
	onChange func()
}

type monitorTile struct {
	widget.BaseWidget
	name    string
	diagram *monitorDiagram
}

func newMonitorDiagram(outputs []DetectedOutput, cfg *Config, onChange func()) fyne.CanvasObject {
	positions := effectiveMonitorRects(outputs, cfg.Outputs)
	minX, minY, maxX, maxY := diagramBounds(positions)
	spanW, spanH := maxX-minX, maxY-minY
	if spanW < 1 {
		spanW = 1
	}
	if spanH < 1 {
		spanH = 1
	}
	scale := minFloat32(
		(monitorStageWidth-2*monitorStagePad)/float32(spanW),
		(monitorStageHeight-2*monitorStagePad)/float32(spanH),
	)

	background := canvas.NewRectangle(theme.InputBackgroundColor())
	background.StrokeColor = theme.SeparatorColor()
	background.StrokeWidth = 1
	background.Resize(fyne.NewSize(monitorStageWidth, monitorStageHeight))
	stage := container.NewWithoutLayout(background)
	stage.Resize(fyne.NewSize(monitorStageWidth, monitorStageHeight))
	diagram := &monitorDiagram{stage: stage, minX: minX, minY: minY, scale: scale, cfg: cfg, onChange: onChange}

	for _, output := range outputs {
		rect := positions[output.Name]
		tile := &monitorTile{name: output.Name, diagram: diagram}
		tile.ExtendBaseWidget(tile)
		tile.Move(fyne.NewPos(
			monitorStagePad+float32(rect.x-minX)*scale,
			monitorStagePad+float32(rect.y-minY)*scale,
		))
		tile.Resize(fyne.NewSize(maxFloat32(80, float32(rect.width)*scale), maxFloat32(48, float32(rect.height)*scale)))
		diagram.tiles = append(diagram.tiles, tile)
		stage.Add(tile)
	}
	return stage
}

func (tile *monitorTile) CreateRenderer() fyne.WidgetRenderer {
	background := canvas.NewRectangle(theme.PrimaryColor())
	background.StrokeColor = color.NRGBA{R: 255, G: 255, B: 255, A: 180}
	background.StrokeWidth = 2
	name := canvas.NewText(tile.name, color.White)
	name.Alignment = fyne.TextAlignCenter
	name.TextStyle = fyne.TextStyle{Bold: true}
	hint := canvas.NewText("drag to arrange", color.NRGBA{R: 255, G: 255, B: 255, A: 210})
	hint.Alignment = fyne.TextAlignCenter
	hint.TextSize = 10
	return widget.NewSimpleRenderer(container.NewStack(background, container.NewVBox(layoutSpacer(), name, hint, layoutSpacer())))
}

func layoutSpacer() fyne.CanvasObject {
	return canvas.NewRectangle(color.Transparent)
}

func (tile *monitorTile) Dragged(event *fyne.DragEvent) {
	pos := tile.Position().Add(event.Dragged)
	pos.X = clampFloat32(pos.X, 0, monitorStageWidth-tile.Size().Width)
	pos.Y = clampFloat32(pos.Y, 0, monitorStageHeight-tile.Size().Height)
	tile.Move(pos)
}

func (tile *monitorTile) DragEnd() {
	pos := snapMonitorPosition(tile, tile.diagram.tiles)
	tile.Move(pos)
	x := int(math.Round(float64((pos.X-monitorStagePad)/tile.diagram.scale))) + tile.diagram.minX
	y := int(math.Round(float64((pos.Y-monitorStagePad)/tile.diagram.scale))) + tile.diagram.minY
	settings := tile.diagram.cfg.Outputs[tile.name]
	settings.MirrorOf = ""
	settings.Position = &OutputPosition{X: intPointer(x), Y: intPointer(y)}
	storeOutputSettings(tile.diagram.cfg, tile.name, settings)
	if tile.diagram.onChange != nil {
		tile.diagram.onChange()
	}
}

func snapMonitorPosition(tile *monitorTile, tiles []*monitorTile) fyne.Position {
	pos := tile.Position()
	bestX, bestY := pos.X, pos.Y
	distanceX, distanceY := monitorSnapDistance+1, monitorSnapDistance+1
	for _, other := range tiles {
		if other == tile {
			continue
		}
		xCandidates := []float32{
			other.Position().X,
			other.Position().X + other.Size().Width,
			other.Position().X - tile.Size().Width,
			other.Position().X + other.Size().Width - tile.Size().Width,
		}
		yCandidates := []float32{
			other.Position().Y,
			other.Position().Y + other.Size().Height,
			other.Position().Y - tile.Size().Height,
			other.Position().Y + other.Size().Height - tile.Size().Height,
		}
		for _, candidate := range xCandidates {
			if distance := absFloat32(pos.X - candidate); distance <= monitorSnapDistance && distance < distanceX {
				bestX, distanceX = candidate, distance
			}
		}
		for _, candidate := range yCandidates {
			if distance := absFloat32(pos.Y - candidate); distance <= monitorSnapDistance && distance < distanceY {
				bestY, distanceY = candidate, distance
			}
		}
	}
	return fyne.NewPos(bestX, bestY)
}

func effectiveMonitorRects(outputs []DetectedOutput, settings map[string]OutputSettings) map[string]logicalRect {
	rects := make(map[string]logicalRect, len(outputs))
	for _, output := range outputs {
		rects[output.Name] = logicalRect{x: output.X, y: output.Y, width: output.Width, height: output.Height}
	}
	// Repeat to resolve relative chains regardless of connector sort order.
	for range outputs {
		for _, output := range outputs {
			rect := rects[output.Name]
			setting := settings[output.Name]
			if setting.MirrorOf != "" {
				if target, ok := rects[setting.MirrorOf]; ok {
					rect.x, rect.y = target.x, target.y
				}
			} else if position := setting.Position; position != nil {
				switch {
				case position.X != nil && position.Y != nil:
					rect.x, rect.y = *position.X, *position.Y
				case position.RightOf != "":
					if target, ok := rects[position.RightOf]; ok {
						rect.x, rect.y = target.x+target.width, target.y
					}
				case position.LeftOf != "":
					if target, ok := rects[position.LeftOf]; ok {
						rect.x, rect.y = target.x-rect.width, target.y
					}
				case position.Above != "":
					if target, ok := rects[position.Above]; ok {
						rect.x, rect.y = target.x, target.y-rect.height
					}
				case position.Below != "":
					if target, ok := rects[position.Below]; ok {
						rect.x, rect.y = target.x, target.y+target.height
					}
				}
			}
			rects[output.Name] = rect
		}
	}
	return rects
}

func diagramBounds(rects map[string]logicalRect) (minX, minY, maxX, maxY int) {
	first := true
	for _, rect := range rects {
		if first {
			minX, minY, maxX, maxY = rect.x, rect.y, rect.x+rect.width, rect.y+rect.height
			first = false
			continue
		}
		minX, minY = minInt(minX, rect.x), minInt(minY, rect.y)
		maxX, maxY = maxInt(maxX, rect.x+rect.width), maxInt(maxY, rect.y+rect.height)
	}
	return
}

func intPointer(value int) *int { return &value }
func minInt(a, b int) int {
	if a < b {
		return a
	}
	return b
}
func maxInt(a, b int) int {
	if a > b {
		return a
	}
	return b
}
func minFloat32(a, b float32) float32 {
	if a < b {
		return a
	}
	return b
}
func maxFloat32(a, b float32) float32 {
	if a > b {
		return a
	}
	return b
}
func absFloat32(value float32) float32 {
	if value < 0 {
		return -value
	}
	return value
}
func clampFloat32(value, low, high float32) float32 { return maxFloat32(low, minFloat32(value, high)) }

func monitorDescription(output DetectedOutput) string {
	if output.Make == "" && output.Model == "" {
		return fmt.Sprintf("%s — %d×%d", output.Name, output.Width, output.Height)
	}
	return fmt.Sprintf("%s — %s %s — %d×%d", output.Name, output.Make, output.Model, output.Width, output.Height)
}
