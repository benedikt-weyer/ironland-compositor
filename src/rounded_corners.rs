//! GPU-side rounded corners for window content.
//!
//! Rounding is applied only to each window's root (toplevel) surface texture
//! (see `shell::element::WindowElement::render_elements`), via a GLES
//! fragment shader that zeroes alpha outside a rounded-rect mask. Because
//! compositing is standard "over" alpha blending, the cut corners simply let
//! whatever was already drawn behind (another window, the wallpaper) show
//! through - no offscreen pass or stencil trickery required.
//!
//! This only works where a real `GlesTexture` is reachable: the winit
//! backend's `GlesRenderer` directly, and the udev backend's per-GPU
//! `GlesRenderer` that the multi-GPU `MultiRenderer`/`MultiFrame` wrap
//! internally (see [`GlesCapable`]/[`AsGlesFrame`]). When a texture can't be
//! recovered this way (currently: never, for the two renderers this
//! compositor uses, but the fallback exists for robustness) the surface is
//! drawn unrounded rather than failing.

use std::{any::Any, cell::Cell};

use smithay::{
    backend::renderer::{
        ImportAll, Renderer, Texture,
        element::{
            Element, Id, Kind, RenderElement, UnderlyingStorage,
            surface::{WaylandSurfaceRenderElement, WaylandSurfaceTexture},
        },
        gles::{
            GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform, UniformName, UniformType,
        },
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{
        Buffer as BufferCoords, Physical, Point, Rectangle, Scale, Transform,
        user_data::UserDataMap,
    },
};

const SHADER: &str = include_str!("../resources/rounded_corners.frag");

/// Renderers from which a [`GlesRenderer`] (and, per-draw-call, a
/// [`GlesFrame`]) can be recovered, regardless of whether they *are* one
/// (winit's backend) or merely wrap one (udev's multi-GPU backend).
///
/// `gles_frame` is a plain generic method - rather than a `where` bound
/// expressed as `for<'f, 'b> Self::Frame<'f, 'b>: SomeTrait<'f, 'b>` - to
/// sidestep a rustc limitation combining higher-ranked bounds with GATs
/// (https://github.com/rust-lang/rust/issues/100013), which the more
/// "natural" phrasing of this trait ran straight into.
pub trait GlesCapable: Renderer {
    fn gles_renderer(&mut self) -> &mut GlesRenderer;

    fn gles_frame<'a, 'frame, 'buffer>(
        frame: &'a mut Self::Frame<'frame, 'buffer>,
    ) -> &'a mut GlesFrame<'frame, 'buffer>;

    /// Lift a [`GlesError`] into this renderer's own error type.
    fn map_gles_error(err: GlesError) -> Self::Error;
}

impl GlesCapable for GlesRenderer {
    fn gles_renderer(&mut self) -> &mut GlesRenderer {
        self
    }

    fn gles_frame<'a, 'frame, 'buffer>(
        frame: &'a mut <Self as smithay::backend::renderer::RendererSuper>::Frame<'frame, 'buffer>,
    ) -> &'a mut GlesFrame<'frame, 'buffer> {
        frame
    }

    fn map_gles_error(err: GlesError) -> GlesError {
        err
    }
}

/// The compiled corner-rounding shader, cached per [`GlesRenderer`] (see
/// [`corners_program`]) since compiling requires a current GL context and
/// each GPU in a multi-GPU setup has its own renderer/context.
#[derive(Debug, Clone)]
struct CornersProgram(GlesTexProgram);

/// Compiles and caches (in `renderer`'s EGL context user data) the
/// corner-rounding shader, returning the cached copy on subsequent calls.
pub fn corners_program(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    if let Some(program) = renderer.egl_context().user_data().get::<CornersProgram>() {
        return Ok(program.0.clone());
    }
    let program = renderer.compile_custom_texture_shader(
        SHADER,
        &[
            UniformName::new("size", UniformType::_2f),
            UniformName::new("radius", UniformType::_1f),
        ],
    )?;
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| CornersProgram(program.clone()));
    Ok(program)
}

/// The corner radius currently in effect, threaded from `AnvilState::config`
/// through to `WindowElement::render_elements` (see [`set_current`]) - a
/// thread-local rather than a render-call parameter because
/// `AsRenderElements::render_elements`'s signature is fixed by smithay and
/// has no room for compositor-specific config.
#[derive(Debug, Clone, Copy, Default)]
pub struct CornersConfig {
    pub enabled: bool,
    pub radius: f32,
}

thread_local! {
    static CURRENT: Cell<CornersConfig> = const { Cell::new(CornersConfig { enabled: false, radius: 0.0 }) };
}

/// Sets the corner radius for the render elements about to be built. Call
/// once per output, before descending into window render-element
/// construction.
pub fn set_current(config: CornersConfig) {
    CURRENT.with(|cell| cell.set(config));
}

pub fn current() -> CornersConfig {
    CURRENT.with(|cell| cell.get())
}

/// Wraps a window's root-surface [`WaylandSurfaceRenderElement`], rounding
/// its corners on draw via a custom GLES texture shader.
pub struct RoundedWindowRenderElement<R: Renderer> {
    inner: WaylandSurfaceRenderElement<R>,
    program: GlesTexProgram,
    radius: f32,
}

impl<R: Renderer> RoundedWindowRenderElement<R> {
    pub fn new(
        inner: WaylandSurfaceRenderElement<R>,
        program: GlesTexProgram,
        radius: f32,
    ) -> Self {
        Self {
            inner,
            program,
            radius,
        }
    }
}

impl<R> Element for RoundedWindowRenderElement<R>
where
    R: Renderer + ImportAll,
    R::TextureId: 'static,
{
    fn id(&self) -> &Id {
        Element::id(&self.inner)
    }

    fn current_commit(&self) -> CommitCounter {
        Element::current_commit(&self.inner)
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        Element::geometry(&self.inner, scale)
    }

    fn src(&self) -> Rectangle<f64, BufferCoords> {
        Element::src(&self.inner)
    }

    fn transform(&self) -> Transform {
        Element::transform(&self.inner)
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        Element::damage_since(&self.inner, scale, commit)
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // The rounded corners are no longer opaque; rather than shrink the
        // inner element's opaque rectangle by the radius on every side,
        // just skip the occlusion-culling optimization for this element.
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        Element::alpha(&self.inner)
    }

    fn kind(&self) -> Kind {
        Element::kind(&self.inner)
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        Element::location(&self.inner, scale)
    }
}

impl<R> RenderElement<R> for RoundedWindowRenderElement<R>
where
    R: Renderer + ImportAll + GlesCapable,
    R::TextureId: Texture + 'static,
{
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error> {
        let texture = match self.inner.texture() {
            WaylandSurfaceTexture::Texture(texture) => {
                (texture as &dyn Any)
                    .downcast_ref::<smithay::backend::renderer::gles::GlesTexture>()
            }
            WaylandSurfaceTexture::SolidColor(_) => None,
        };

        let Some(texture) = texture else {
            // No GLES texture to shade (a single-pixel-color buffer, or a
            // renderer this shader can't reach into) - draw unrounded.
            return self
                .inner
                .draw(frame, src, dst, damage, opaque_regions, cache);
        };

        let uniforms = [
            Uniform::new("size", (dst.size.w as f32, dst.size.h as f32)),
            Uniform::new("radius", self.radius),
        ];

        R::gles_frame(frame)
            .render_texture_from_to(
                texture,
                src,
                dst,
                damage,
                opaque_regions,
                Element::transform(&self.inner),
                Element::alpha(&self.inner),
                Some(&self.program),
                &uniforms,
            )
            .map_err(R::map_gles_error)
    }

    #[inline]
    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        // Rounded windows are always composited through GL, never scanned
        // out directly as a drm plane.
        None
    }
}
