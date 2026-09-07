//! GPU-side highlight border drawn around the currently focused window only.
//!
//! Unlike [`crate::rounded_corners`], which has to hook deep into
//! `WindowElement::render_elements` (a signature fixed by smithay, with no
//! room for compositor-specific config or "is this the focused window?"
//! context), the border is built at the shared `render::output_elements`
//! composition point instead, where the config and the focused window are
//! both plain parameters - no thread-local plumbing needed.
//!
//! Drawn with a GLES pixel shader (no texture to sample, just a rounded-rect
//! ring), compiled once per `GlesRenderer` and cached the same way
//! `rounded_corners::corners_program` caches its texture shader.

use ironland_config::BorderSettings;
use smithay::{
    backend::renderer::{
        ImportAll, Renderer,
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{GlesError, GlesPixelProgram, GlesRenderer, Uniform, UniformName, UniformType},
        utils::{CommitCounter, OpaqueRegions},
    },
    utils::{
        Buffer as BufferCoords, Logical, Physical, Point, Rectangle, Scale, Size, Transform,
        user_data::UserDataMap,
    },
};

use crate::rounded_corners::GlesCapable;

const SHADER: &str = include_str!("../resources/border.frag");

#[derive(Debug, Clone)]
struct BorderProgram(GlesPixelProgram);

/// Compiles and caches (in `renderer`'s EGL context user data) the border
/// pixel shader, returning the cached copy on subsequent calls.
fn border_program(renderer: &mut GlesRenderer) -> Result<GlesPixelProgram, GlesError> {
    if let Some(program) = renderer.egl_context().user_data().get::<BorderProgram>() {
        return Ok(program.0.clone());
    }
    let program = renderer.compile_custom_pixel_shader(
        SHADER,
        &[
            UniformName::new("thickness", UniformType::_1f),
            UniformName::new("innerRadius", UniformType::_1f),
            UniformName::new("color1", UniformType::_4f),
            UniformName::new("color2", UniformType::_4f),
            UniformName::new("angle", UniformType::_1f),
        ],
    )?;
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| BorderProgram(program.clone()));
    Ok(program)
}

/// `#rrggbb` or `#rrggbbaa` to straight-alpha `[r, g, b, a]` floats in
/// `0.0..=1.0`. `None` on anything else, which callers treat as "no
/// border" rather than falling back to a guessed color.
fn parse_hex(s: &str) -> Option<[f32; 4]> {
    let s = s.strip_prefix('#')?;
    let channel = |i: usize| -> Option<f32> { Some(u8::from_str_radix(s.get(i..i + 2)?, 16).ok()? as f32 / 255.0) };
    match s.len() {
        6 => Some([channel(0)?, channel(2)?, channel(4)?, 1.0]),
        8 => Some([channel(0)?, channel(2)?, channel(4)?, channel(6)?]),
        _ => None,
    }
}

/// A rounded-rect ring drawn around `area` (already inflated by the
/// configured thickness - see [`build`]).
pub struct BorderRenderElement {
    id: Id,
    commit: CommitCounter,
    area: Rectangle<i32, Logical>,
    program: GlesPixelProgram,
    color1: [f32; 4],
    color2: [f32; 4],
    angle: f32,
    thickness: f32,
    radius: f32,
}

impl Element for BorderRenderElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.area.to_physical_precise_round(scale)
    }

    fn src(&self) -> Rectangle<f64, BufferCoords> {
        Rectangle::from_size(self.area.size.to_f64().to_buffer(1.0, Transform::Normal))
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // The ring has a transparent hole in the middle (and, with a
        // corner radius, transparent corners), so nothing here is opaque.
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        1.0
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl<R> RenderElement<R> for BorderRenderElement
where
    R: Renderer + ImportAll + GlesCapable,
{
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error> {
        let uniforms = [
            Uniform::new("thickness", self.thickness),
            Uniform::new("innerRadius", self.radius),
            Uniform::new("color1", self.color1),
            Uniform::new("color2", self.color2),
            Uniform::new("angle", self.angle),
        ];

        R::gles_frame(frame)
            .render_pixel_shader_to(
                &self.program,
                src,
                dst,
                self.area.size.to_buffer(1, Transform::Normal),
                Some(damage),
                1.0,
                &uniforms,
            )
            .map_err(R::map_gles_error)
    }

    #[inline]
    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

/// Builds the border element for `window_rect` (the focused window's own
/// on-screen bounds, output-local logical coordinates), or `None` if the
/// border is disabled, has no thickness, or its color doesn't parse.
/// `corner_radius` should be the window's own rounded-corner radius (0 if
/// corners are disabled), so the border follows it.
pub fn build<R>(
    renderer: &mut R,
    window_rect: Rectangle<i32, Logical>,
    settings: &BorderSettings,
    corner_radius: f32,
) -> Option<BorderRenderElement>
where
    R: Renderer + ImportAll + GlesCapable,
{
    if !settings.enabled || settings.thickness == 0 {
        return None;
    }
    let color1 = parse_hex(&settings.color)?;
    let color2 = settings
        .gradient_color
        .as_deref()
        .map(parse_hex)
        .unwrap_or(Some(color1))?;

    let program = border_program(renderer.gles_renderer()).ok()?;
    let thickness = settings.thickness as i32;
    let area = Rectangle::new(
        window_rect.loc - Point::from((thickness, thickness)),
        window_rect.size + Size::from((2 * thickness, 2 * thickness)),
    );

    Some(BorderRenderElement {
        id: Id::new(),
        commit: CommitCounter::default(),
        area,
        program,
        color1,
        color2,
        angle: settings.angle.to_radians(),
        thickness: thickness as f32,
        radius: corner_radius,
    })
}
