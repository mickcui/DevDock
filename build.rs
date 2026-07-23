#[cfg(windows)]
fn main() {
    use std::{env, fs, fs::File, path::PathBuf};

    let svg = fs::read("assets/logo.svg").expect("failed to read application logo");
    let tree = resvg::usvg::Tree::from_data(&svg, &Default::default())
        .expect("failed to parse application logo");
    let mut icon = ico::IconDir::new(ico::ResourceType::Icon);

    for size in [16, 24, 32, 48, 64, 128, 256] {
        let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).expect("invalid icon size");
        let scale = size as f32 / tree.size().width();
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );

        let mut rgba = pixmap.take();
        for pixel in rgba.chunks_exact_mut(4) {
            let alpha = u16::from(pixel[3]);
            if alpha > 0 {
                for channel in &mut pixel[..3] {
                    *channel = (u16::from(*channel) * 255 / alpha).min(255) as u8;
                }
            }
        }
        let image = ico::IconImage::from_rgba_data(size, size, rgba);
        icon.add_entry(ico::IconDirEntry::encode(&image).expect("failed to encode icon image"));
    }

    let icon_path =
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is not set")).join("devdock.ico");
    icon.write(File::create(&icon_path).expect("failed to create icon file"))
        .expect("failed to write icon file");

    winresource::WindowsResource::new()
        .set_icon(icon_path.to_str().expect("icon path is not UTF-8"))
        .compile()
        .expect("failed to embed Windows application icon");
    println!("cargo:rerun-if-changed=assets/logo.svg");
}

#[cfg(not(windows))]
fn main() {}
