// Writes a half-float RGBA EXR (ZIP) from a PNG: a texture-sized EXR for timing, not a fixture.
use exr::prelude::*;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let img = image::open(&a[1]).expect("read png").to_rgba16();
    let (w, h) = img.dimensions();
    let px = |x: usize, y: usize| {
        let p = img.get_pixel(x as u32, y as u32).0;
        let f = |v: u16| f16::from_f32((v as f32 / 65535.0).powf(2.2));
        (f(p[0]), f(p[1]), f(p[2]), f16::from_f32(p[3] as f32 / 65535.0))
    };
    let layer = Layer::new((w as usize, h as usize), LayerAttributes::default(),
        Encoding { compression: Compression::ZIP16, ..Encoding::default() },
        SpecificChannels::rgba(|p: Vec2<usize>| px(p.x(), p.y())));
    Image::from_layer(layer).write().to_file(&a[2]).expect("write exr");
}
