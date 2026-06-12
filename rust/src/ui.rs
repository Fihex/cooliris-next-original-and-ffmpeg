// Minimal GPU text layer (glyphon) for the toolbar: the Open-button label and the loaded/total
// readout. Buffers are rebuilt every frame in prepare(); render() draws what prepare() staged.

use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};

/// One run of text to draw this frame (top-left origin, in pixels).
pub struct Line {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub color: [u8; 4],
}

pub struct Ui {
    font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    renderer: TextRenderer,
    viewport: Viewport,
}

impl Ui {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Ui {
        let cache = Cache::new(device);
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let renderer =
            TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        let viewport = Viewport::new(device, &cache);
        Ui {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            atlas,
            renderer,
            viewport,
        }
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        lines: &[Line],
    ) {
        self.viewport.update(queue, Resolution { width, height });
        let mut buffers = Vec::with_capacity(lines.len());
        for l in lines {
            let mut b = Buffer::new(&mut self.font_system, Metrics::new(l.size, l.size * 1.25));
            b.set_size(&mut self.font_system, Some(width as f32), Some(height as f32));
            b.set_text(
                &mut self.font_system,
                &l.text,
                Attrs::new().family(Family::SansSerif),
                Shaping::Advanced,
            );
            b.shape_until_scroll(&mut self.font_system, false);
            buffers.push(b);
        }
        let areas: Vec<TextArea> = lines
            .iter()
            .zip(&buffers)
            .map(|(l, b)| TextArea {
                buffer: b,
                left: l.x,
                top: l.y,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: width as i32,
                    bottom: height as i32,
                },
                default_color: Color::rgba(l.color[0], l.color[1], l.color[2], l.color[3]),
                custom_glyphs: &[],
            })
            .collect();
        let _ = self.renderer.prepare(
            device,
            queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas,
            &mut self.swash_cache,
        );
    }

    pub fn render<'a>(&'a self, rp: &mut wgpu::RenderPass<'a>) {
        let _ = self.renderer.render(&self.atlas, &self.viewport, rp);
    }
}
