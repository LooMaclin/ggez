use glyph_brush::GlyphPositioner;
use glyph_brush::{self, Layout, SectionText, VariedSection};
pub use glyph_brush::{rusttype::Scale, FontId, HorizontalAlign as Align};
use mint;
use std::borrow::Cow;
use std::f32;
use std::fmt;
use std::io::Read;
use std::path;
use std::sync::{Arc, RwLock};

use super::*;

// TODO: consider adding bits from example to docs.

/// Default size for fonts.
pub const DEFAULT_FONT_SCALE: f32 = 16.0;

/// A handle referring to a loaded Truetype font.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Font {
    font_id: FontId,
    // TODO: Add DebugId?  It makes Font::default() less convenient.
}

/// A piece of text with optional color, font and font scale information.
/// Drawing text generally involves one or more of these.
/// These options take precedence over any similar field/argument.
/// Can be implicitly constructed from `String`, `(String, Color)`, and `(String, FontId, Scale)`.
///
/// TODO: Construction should be full builder pattern, if it's not already.
#[derive(Clone, Debug)]
pub struct TextFragment {
    /// Text string itself.
    pub text: String,
    /// Fragment's color, defaults to text's color.
    pub color: Option<Color>,
    /// Fragment's font, defaults to text's font.
    pub font: Option<Font>,
    /// Fragment's scale, defaults to text's scale.
    pub scale: Option<Scale>,
}

impl Default for TextFragment {
    fn default() -> Self {
        TextFragment {
            text: "".into(),
            color: None,
            font: None,
            scale: None,
        }
    }
}

impl TextFragment {
    /// Creates a new fragment from `String` or `&str`.
    pub fn new<T: Into<Self>>(text: T) -> Self {
        text.into()
    }

    /// Set fragment's color, overrides text's color.
    pub fn color(mut self, color: Color) -> TextFragment {
        self.color = Some(color);
        self
    }

    /// Set fragment's font, overrides text's font.
    pub fn font(mut self, font: Font) -> TextFragment {
        self.font = Some(font);
        self
    }

    /// Set fragment's scale, overrides text's scale.
    pub fn scale(mut self, scale: Scale) -> TextFragment {
        self.scale = Some(scale);
        self
    }
}

impl<'a> From<&'a str> for TextFragment {
    fn from(text: &'a str) -> TextFragment {
        TextFragment {
            text: text.to_owned(),
            ..Default::default()
        }
    }
}

impl From<char> for TextFragment {
    fn from(ch: char) -> TextFragment {
        TextFragment {
            text: ch.to_string(),
            ..Default::default()
        }
    }
}

impl From<String> for TextFragment {
    fn from(text: String) -> TextFragment {
        TextFragment {
            text,
            ..Default::default()
        }
    }
}

// TODO: Scale ergonomics need to be better
impl<T> From<(T, Font, f32)> for TextFragment
where
    T: Into<TextFragment>,
{
    fn from((text, font, scale): (T, Font, f32)) -> TextFragment {
        text.into().font(font).scale(Scale::uniform(scale))
    }
}

/// Cached font metrics that we can keep attached to a `Text`
/// so we don't have to keep recalculating them.
#[derive(Clone, Debug)]
struct CachedMetrics {
    string: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

impl Default for CachedMetrics {
    fn default() -> CachedMetrics {
        CachedMetrics {
            string: None,
            width: None,
            height: None,
        }
    }
}

/// Drawable text object.  Essentially a list of [`TextFragment`](struct.TextFragment.html)'s
/// and some metrics information.
///
/// It implements [`Drawable`](trait.Drawable.html) so it can be drawn immediately with
/// [`graphics::draw()`](fn.draw.html), or many of them can be queued with [`graphics::queue_text()`](fn.queue_text.html)
/// and then all drawn at once with [`graphics::draw_queued_text()`](fn.draw_queued_text.html).
#[derive(Debug)]
pub struct Text {
    fragments: Vec<TextFragment>,
    // TODO: make it do something, maybe.
    blend_mode: Option<BlendMode>,
    bounds: Point2,
    layout: Layout<glyph_brush::BuiltInLineBreaker>,
    font_id: FontId,
    font_scale: Scale,
    cached_metrics: Arc<RwLock<CachedMetrics>>,
}

/// This has to be explicit. Derived `Clone` clones the `Arc`, so clones end up
/// sharing the metrics instead of the proper behavior which is to have different
/// ones for each `Text` object, since `Text` may be mutated.
///
/// TODO: Can we just ditch the `Arc` entirely then?
/// ...do we ever even WRITE the actual metrics to cached_metrics?
impl Clone for Text {
    fn clone(&self) -> Self {
        Text {
            fragments: self.fragments.clone(),
            blend_mode: self.blend_mode,
            bounds: self.bounds,
            layout: self.layout,
            font_id: self.font_id,
            font_scale: self.font_scale,
            cached_metrics: Arc::new(RwLock::new(CachedMetrics::default())),
        }
    }
}

impl Default for Text {
    fn default() -> Self {
        Text {
            fragments: Vec::new(),
            blend_mode: None,
            bounds: Point2::new(f32::INFINITY, f32::INFINITY),
            layout: Layout::default(),
            font_id: FontId::default(),
            font_scale: Scale::uniform(DEFAULT_FONT_SCALE),
            cached_metrics: Arc::new(RwLock::new(CachedMetrics::default())),
        }
    }
}

impl Text {
    /// Creates a `Text` from a `TextFragment`.
    ///
    /// ```rust
    /// # use ggez::graphics::Text;
    /// # fn main() {
    /// let text = Text::new("foo");
    /// # }
    /// ```
    pub fn new<F>(fragment: F) -> Text
    where
        F: Into<TextFragment>,
    {
        let mut text = Text::default();
        let _ = text.add(fragment);
        text
    }

    /// Appends a `TextFragment` to the `Text`.
    pub fn add<F>(&mut self, fragment: F) -> &mut Text
    where
        F: Into<TextFragment>,
    {
        self.fragments.push(fragment.into());
        self.invalidate_cached_metrics();
        self
    }

    /// Returns a read-only slice of all `TextFragment`'s.
    pub fn fragments(&self) -> &[TextFragment] {
        &self.fragments
    }

    /// Returns a mutable slice with all fragments.
    pub fn fragments_mut(&mut self) -> &mut [TextFragment] {
        &mut self.fragments
    }

    /// Specifies rectangular dimensions to try and fit contents inside of,
    /// by wrapping, and alignment within the bounds.
    pub fn set_bounds<P>(&mut self, bounds: P, alignment: Align) -> &mut Text
    where
        P: Into<mint::Point2<f32>>,
    {
        self.bounds = Point2::from(bounds.into());
        if self.bounds.x == f32::INFINITY {
            // Layouts don't make any sense if we don't wrap text at all.
            self.layout = Layout::default();
        } else {
            self.layout = self.layout.h_align(alignment);
        }
        self.invalidate_cached_metrics();
        self
    }

    /// Specifies text's font and font scale; used for fragments that don't have their own.
    pub fn set_font(&mut self, font: Font, font_scale: Scale) -> &mut Text {
        self.font_id = font.font_id;
        self.font_scale = font_scale;
        self.invalidate_cached_metrics();
        self
    }

    /// Converts `Text` to a type `gfx_glyph` can understand and queue.
    fn generate_varied_section(
        &self,
        relative_dest: Point2,
        color: Option<Color>,
    ) -> VariedSection {
        let mut sections = Vec::with_capacity(self.fragments.len());
        for fragment in &self.fragments {
            let color = match fragment.color {
                Some(c) => c,
                None => match color {
                    Some(c) => c,
                    None => WHITE,
                },
            };
            let font_id = match fragment.font {
                Some(font) => font.font_id,
                None => self.font_id,
            };
            let scale = match fragment.scale {
                Some(scale) => scale,
                None => self.font_scale,
            };
            sections.push(SectionText {
                text: &fragment.text,
                color: <[f32; 4]>::from(color),
                font_id,
                scale,
            });
        }
        let relative_dest_x = {
            // This positions text within bounds with relative_dest being to the left, always.
            let mut dest_x = relative_dest.x;
            if self.bounds.x != f32::INFINITY {
                use glyph_brush::Layout::Wrap;
                if let Wrap { h_align, .. } = self.layout {
                    match h_align {
                        Align::Center => dest_x += self.bounds.x * 0.5,
                        Align::Right => dest_x += self.bounds.x,
                        _ => (),
                    }
                }
            }
            dest_x
        };
        let relative_dest = (relative_dest_x, relative_dest.y);
        VariedSection {
            screen_position: relative_dest,
            bounds: (self.bounds.x, self.bounds.y),
            //z: f32,
            layout: self.layout,
            text: sections,
            ..Default::default()
        }
    }

    fn invalidate_cached_metrics(&mut self) {
        if let Ok(mut metrics) = self.cached_metrics.write() {
            *metrics = CachedMetrics::default();
            // Returning early avoids a double-borrow in the "else"
            // part.
            return;
        }
        warn!("Cached metrics RwLock has been poisoned.");
        self.cached_metrics = Arc::new(RwLock::new(CachedMetrics::default()));
    }

    /// Returns the string that the text represents.
    pub fn contents(&self) -> String {
        if let Ok(metrics) = self.cached_metrics.read() {
            if let Some(ref string) = metrics.string {
                return string.clone();
            }
        }
        let mut string_accm = String::new();
        for frg in &self.fragments {
            string_accm += &frg.text;
        }
        if let Ok(mut metrics) = self.cached_metrics.write() {
            metrics.string = Some(string_accm.clone());
        }
        string_accm
    }

    // /// Calculates, caches, and returns width and height of formatted and wrapped text.
    // fn calculate_dimensions(&self, context: &Context) -> (u32, u32) {
    //     let mut max_width = 0;
    //     let mut max_height = 0;
    //     {
    //         let varied_section = self.generate_varied_section(Point2::new(0.0, 0.0), None);
    //         let glyphed_section_texts = self
    //             .layout

    //             .calculate_glyphs(context.gfx_context.fonts, &varied_section);
    //             // .calculate_glyphs(context.gfx_context.glyph_brush.fonts(), &varied_section);
    //         for glyphed_section_text in &glyphed_section_texts {
    //             let (ref positioned_glyph, ..) = glyphed_section_text;
    //             if let Some(rect) = positioned_glyph.pixel_bounding_box() {
    //                 if rect.max.x > max_width {
    //                     max_width = rect.max.x;
    //                 }
    //                 if rect.max.y > max_height {
    //                     max_height = rect.max.y;
    //                 }
    //             }
    //         }
    //     }
    //     let (width, height) = (max_width as u32, max_height as u32);
    //     if let Ok(mut metrics) = self.cached_metrics.write() {
    //         metrics.width = Some(width);
    //         metrics.height = Some(height);
    //     }
    //     (width, height)
    // }

    // /// Returns the width and height of the formatted and wrapped text.
    // ///
    // /// TODO: Should these return f32 rather than u32?
    // pub fn dimensions(&self, context: &Context) -> (u32, u32) {
    //     if let Ok(metrics) = self.cached_metrics.read() {
    //         if let (Some(width), Some(height)) = (metrics.width, metrics.height) {
    //             return (width, height);
    //         }
    //     }
    //     self.calculate_dimensions(context)
    // }

    // /// Returns the width of formatted and wrapped text, in screen coordinates.
    // pub fn width(&self, context: &Context) -> u32 {
    //     self.dimensions(context).0
    // }

    // /// Returns the height of formatted and wrapped text, in screen coordinates.
    // pub fn height(&self, context: &Context) -> u32 {
    //     self.dimensions(context).1
    // }
}

impl Drawable for Text {
    fn draw<D>(&self, ctx: &mut Context, param: D) -> GameResult
    where
        D: Into<DrawParam>,
    {
        let param = param.into();
        // Converts fraction-of-bounding-box to screen coordinates, as required by `draw_queued()`.
        // TODO: Fix for DrawTransform
        // let offset = Point2::new(
        //     param.offset.x * self.width(ctx) as f32,
        //     param.offset.y * self.height(ctx) as f32,
        // );
        // let param = param.offset(offset);
        queue_text(ctx, self, Point2::new(0.0, 0.0), Some(param.color));
        draw_queued_text(ctx, param)
    }

    fn set_blend_mode(&mut self, mode: Option<BlendMode>) {
        self.blend_mode = mode;
    }

    fn blend_mode(&self) -> Option<BlendMode> {
        self.blend_mode
    }
}

impl Font {
    /// Load a new TTF font from the given file.
    pub fn new<P>(context: &mut Context, path: P) -> GameResult<Font>
    where
        P: AsRef<path::Path> + fmt::Debug,
    {
        use crate::filesystem;
        let mut stream = filesystem::open(context, path.as_ref())?;
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf)?;

        // TODO: DPI; see winit #548.  Also need point size, pixels, etc...
        Font::new_glyph_font_bytes(context, &buf)
    }

    /// Loads a new TrueType font from given bytes and into a `gfx::GlyphBrush` owned
    /// by the `Context`.
    pub fn new_glyph_font_bytes(context: &mut Context, bytes: &[u8]) -> GameResult<Self> {
        // Take a Cow here to avoid this clone where unnecessary?
        // Nah, let's not complicate things more than necessary.
        let v = bytes.to_vec();
        let font_id = context.gfx_context.glyph_brush.add_font_bytes(v);

        Ok(Font { font_id })
    }

    /// Returns the baked-in bytes of default font (currently `DejaVuSerif.ttf`).
    pub(crate) fn default_font_bytes() -> &'static [u8] {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/resources/DejaVuSerif.ttf"
        ))
    }
}

impl Default for Font {
    fn default() -> Self {
        Font { font_id: FontId(0) }
    }
}

/// Queues the `Text` to be drawn by [`draw_queued()`](fn.draw_queued.html).
/// `relative_dest` is relative to the [`DrawParam::dest`](struct.DrawParam.html#structfield.dest)
/// passed to `draw_queued()`./ Note, any `Text` drawn via [`graphics::draw()`](fn.draw.html)
/// will also draw the queue.
pub fn queue_text<P>(context: &mut Context, batch: &Text, relative_dest: P, color: Option<Color>)
where
    P: Into<mint::Point2<f32>>,
{
    let p = Point2::from(relative_dest.into());
    let varied_section = batch.generate_varied_section(p, color);
    context.gfx_context.glyph_brush.queue(varied_section);
}

/// Exposes `gfx_glyph`'s `GlyphBrush::queue()` and `GlyphBrush::queue_custom_layout()`,
/// in case `ggez`' API is insufficient.
pub fn queue_text_raw<'a, S, G>(context: &mut Context, section: S, custom_layout: Option<&G>)
where
    S: Into<Cow<'a, VariedSection<'a>>>,
    G: GlyphPositioner,
{
    let brush = &mut context.gfx_context.glyph_brush;
    match custom_layout {
        Some(layout) => brush.queue_custom_layout(section, layout),
        None => brush.queue(section),
    }
}

/// Draws all of the [`Text`](struct.Text.html)s added via [`queue_text()`](fn.queue_text.html).
///
/// `DrawParam` apply to everything in the queue; offset is in screen coordinates;
/// color is ignored - specify it when `queue_text()`ing instead.
pub fn draw_queued_text<D>(ctx: &mut Context, param: D) -> GameResult
where
    D: Into<DrawParam>,
{
    let param: DrawParam = param.into();
    let screen_rect = screen_coordinates(ctx);

    let (screen_w, screen_h) = (screen_rect.w, screen_rect.h);

    let gb = &mut ctx.gfx_context.glyph_brush;
    let encoder = &mut ctx.gfx_context.encoder;
    let gc = &ctx.gfx_context.glyph_cache.texture_handle;
    let backend = &ctx.gfx_context.backend_spec;

    let action = gb.process_queued(
        (screen_w as u32, screen_h as u32),
        |rect, tex_data| update_texture::<GlBackendSpec>(backend, encoder, gc, rect, tex_data),
        to_vertex,
    );
    match action {
        Ok(glyph_brush::BrushAction::ReDraw) => {
            let s = ctx.gfx_context.glyph_state.clone();
            draw(ctx, &*s.borrow(), param)?;
        }
        Ok(glyph_brush::BrushAction::Draw(drawparams)) => {
            // Gotta clone the image to avoid double-borrow's.
            // let image = ctx.gfx_context.glyph_cache.clone();
            let spritebatch = ctx.gfx_context.glyph_state.clone();
            let spritebatch = &mut *spritebatch.borrow_mut();
            spritebatch.clear();
            for p in &drawparams {
                // Ignore returned sprite index.
                let _ = spritebatch.add(*p);
            }
            // TODO: Augh, double-borrow
            // let s = ctx.gfx_context.glyph_state.clone();
            // let s = &ctx.gfx_context.glyph_state;
            // ctx.gfx_context.glyph_state.draw(ctx, param)?;
            draw(ctx, &*spritebatch, param)?;
        }
        Err(glyph_brush::BrushError::TextureTooSmall { suggested }) => {
            let (new_width, new_height) = suggested;
            let data = vec![255; 4 * new_width as usize * new_height as usize];
            let new_glyph_cache =
                Image::from_rgba8(ctx, new_width as u16, new_height as u16, &data)?;
            ctx.gfx_context.glyph_cache = new_glyph_cache.clone();
            let spritebatch = ctx.gfx_context.glyph_state.clone();
            let spritebatch = &mut *spritebatch.borrow_mut();
            let _ = spritebatch.set_image(new_glyph_cache);
            ctx.gfx_context
                .glyph_brush
                .resize_texture(new_width, new_height);
        }
    }
    Ok(())
}

fn update_texture<B>(
    backend: &B,
    encoder: &mut gfx::Encoder<B::Resources, B::CommandBuffer>,
    texture: &gfx::handle::RawTexture<B::Resources>,
    rect: glyph_brush::rusttype::Rect<u32>,
    tex_data: &[u8],
) where
    B: BackendSpec,
{
    let offset = [rect.min.x as u16, rect.min.y as u16];
    let size = [rect.width() as u16, rect.height() as u16];
    let info = texture::ImageInfoCommon {
        xoffset: offset[0],
        yoffset: offset[1],
        zoffset: 0,
        width: size[0],
        height: size[1],
        depth: 0,
        format: (),
        mipmap: 0,
    };

    let tex_data_chunks: Vec<[u8; 4]> = tex_data.iter().map(|c| [255, 255, 255, *c]).collect();
    let typed_tex = backend.raw_to_typed_texture(texture.clone());
    encoder
        .update_texture::<<super::BuggoSurfaceFormat as gfx::format::Formatted>::Surface, super::BuggoSurfaceFormat>(
            &typed_tex, None, info, &tex_data_chunks,
        )
        .unwrap();
}

/// I THINK what we're going to need to do is have a
/// `SpriteBatch` that actually does the stuff and stores the
/// UV's and verts and such, while
///
/// Basically, `glyph_brush`'s "`to_vertex`" callback is really
/// `to_quad`; in the default code it
fn to_vertex(v: glyph_brush::GlyphVertex) -> DrawParam {
    let src_rect = Rect {
        x: v.tex_coords.min.x,
        y: v.tex_coords.min.y,
        w: v.tex_coords.max.x - v.tex_coords.min.x,
        h: v.tex_coords.max.y - v.tex_coords.min.y,
    };
    // it LOOKS like pixel_coords are the output coordinates?
    // I'm not sure though...
    let dest_pt = Point2::new(v.pixel_coords.min.x as f32, v.pixel_coords.min.y as f32);
    DrawParam::default()
        .src(src_rect)
        .dest(dest_pt)
        .color(v.color)
}

#[cfg(test)]
mod tests {
    /*
        use super::*;
        #[test]
        fn test_metrics() {
            let f = Font::default_font().expect("Could not get default font");
            assert_eq!(f.height(), 17);
            assert_eq!(f.width("Foo!"), 33);

            // http://www.catipsum.com/index.php
            let text_to_wrap = "Walk on car leaving trail of paw prints on hood and windshield sniff \
                                other cat's butt and hang jaw half open thereafter for give attitude. \
                                Annoy kitten\nbrother with poking. Mrow toy mouse squeak roll over. \
                                Human give me attention meow.";
            let (len, v) = f.wrap(text_to_wrap, 250);
            println!("{} {:?}", len, v);
            assert_eq!(len, 249);

            /*
            let wrapped_text = vec![
                "Walk on car leaving trail of paw prints",
                "on hood and windshield sniff other",
                "cat\'s butt and hang jaw half open",
                "thereafter for give attitude. Annoy",
                "kitten",
                "brother with poking. Mrow toy",
                "mouse squeak roll over. Human give",
                "me attention meow."
            ];
    */
    let wrapped_text = vec![
    "Walk on car leaving trail of paw",
    "prints on hood and windshield",
    "sniff other cat\'s butt and hang jaw",
    "half open thereafter for give",
    "attitude. Annoy kitten",
    "brother with poking. Mrow toy",
    "mouse squeak roll over. Human",
    "give me attention meow.",
    ];

    assert_eq!(&v, &wrapped_text);
    }

    // We sadly can't have this test in the general case because it needs to create a Context,
    // which creates a window, which fails on a headless server like our CI systems.  :/
    //#[test]
    #[allow(dead_code)]
    fn test_wrapping() {
    use conf;
    let c = conf::Conf::new();
    let (ctx, _) = &mut Context::load_from_conf("test_wrapping", "ggez", c)
    .expect("Could not create context?");
    let font = Font::default_font().expect("Could not get default font");
    let text_to_wrap = "Walk on car leaving trail of paw prints on hood and windshield sniff \
    other cat's butt and hang jaw half open thereafter for give attitude. \
    Annoy kitten\nbrother with poking. Mrow toy mouse squeak roll over. \
    Human give me attention meow.";
    let wrap_length = 250;
    let (len, v) = font.wrap(text_to_wrap, wrap_length);
    assert!(len < wrap_length);
    for line in &v {
    let t = Text::new(ctx, line, &font).unwrap();
    println!(
    "Width is claimed to be <= {}, should be <= {}, is {}",
    len,
    wrap_length,
    t.width()
    );
    // Why does this not match?  x_X
    //assert!(t.width() as usize <= len);
    assert!(t.width() as usize <= wrap_length);
    }
    }
     */
}
/*
// Creates a gfx texture with the given data
fn create_texture<F, R>(
    factory: &mut F,
    width: u32,
    height: u32,
) -> Result<(TexSurfaceHandle<R>, TexShaderView<R>), Box<dyn Error>>
where
    R: gfx::Resources,
    F: gfx::Factory<R>,
{
    let kind = texture::Kind::D2(
        width as texture::Size,
        height as texture::Size,
        texture::AaMode::Single,
    );

    let tex = factory.create_texture(
        kind,
        1 as texture::Level,
        gfx::memory::Bind::SHADER_RESOURCE,
        gfx::memory::Usage::Dynamic,
        Some(<TexChannel as format::ChannelTyped>::get_channel_type()),
    )?;

    let view =
        factory.view_texture_as_shader_resource::<TexForm>(&tex, (0, 0), format::Swizzle::new())?;

    Ok((tex, view))
}

// Updates a texture with the given data (used for updating the GlyphCache texture)
#[inline]
fn update_texture<R, C>(
    encoder: &mut gfx::Encoder<R, C>,
    texture: &handle::Texture<R, TexSurface>,
    offset: [u16; 2],
    size: [u16; 2],
    data: &[u8],
) where
    R: gfx::Resources,
    C: gfx::CommandBuffer<R>,
{
    let info = texture::ImageInfoCommon {
        xoffset: offset[0],
        yoffset: offset[1],
        zoffset: 0,
        width: size[0],
        height: size[1],
        depth: 0,
        format: (),
        mipmap: 0,
    };
    encoder
        .update_texture::<TexSurface, TexForm>(texture, None, info, data)
        .unwrap();
}
*/
