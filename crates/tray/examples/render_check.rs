//! Headless SVG render smoke-test. Reads the embedded icon.svg via the
//! same code path the tray uses, writes a PNG to /tmp/icon_preview.png,
//! and prints a couple of sanity stats.
//!
//! Usage:
//!   cargo run -p agentline-tray --example render_check
//!   open /tmp/icon_preview.png

fn main() {
    const SVG: &[u8] = include_bytes!("../assets/icon.svg");
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(SVG, &opt).expect("parse icon.svg");
    let s = tree.size();
    println!("SVG declared size: {} x {}", s.width(), s.height());

    for size in [16u32, 32, 64, 128] {
        let mut pm = resvg::tiny_skia::Pixmap::new(size, size).unwrap();
        let sx = size as f32 / s.width();
        let sy = size as f32 / s.height();
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(sx, sy),
            &mut pm.as_mut(),
        );
        // Quick non-empty check
        let nonzero = pm.data().chunks(4).filter(|p| p[3] != 0).count();
        let total = (size * size) as usize;
        println!("rasterized {size}x{size}: {nonzero}/{total} non-transparent pixels");
        let path = format!("/tmp/icon_preview_{size}.png");
        pm.save_png(&path).unwrap();
        println!("  → {path}");
    }
}
