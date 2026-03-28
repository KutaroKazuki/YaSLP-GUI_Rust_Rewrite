mod app;
mod fetch;
mod models;
mod settings;

use eframe::egui;

fn main() -> eframe::Result<()> {
    // On Linux, winit auto-detects Wayland (via WAYLAND_DISPLAY) or falls
    // back to X11. No extra config needed.

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("YaSLP-GUI — Switch LAN Play")
            .with_inner_size([960.0, 600.0])
            .with_min_inner_size([820.0, 500.0])
            .with_icon(load_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "YaSLP-GUI",
        options,
        Box::new(|cc| {
            // Use Inter font for a modern look
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "Inter".into(),
                egui::FontData::from_static(include_bytes!("../assets/Inter-Regular.ttf")).into(),
            );
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "Inter".into());
            cc.egui_ctx.set_fonts(fonts);

            Ok(Box::new(app::YaSLPApp::default()))
        }),
    )
}

fn load_icon() -> egui::IconData {
    // 32x32 icon: purple circle with a ⊕ symbol painted at runtime
    // We generate a simple RGBA pixel buffer
    let size = 32u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let cx = size as f32 / 2.0;
    let cy = size as f32 / 2.0;
    let r = (size as f32 / 2.0) - 1.0;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let idx = ((y * size + x) * 4) as usize;
            if dist <= r {
                // purple-blue gradient
                let t = dist / r;
                rgba[idx] = lerp(124, 60, t);     // R
                rgba[idx + 1] = lerp(131, 80, t); // G
                rgba[idx + 2] = lerp(253, 200, t);// B
                rgba[idx + 3] = 255;              // A
            }
        }
    }
    egui::IconData {
        rgba,
        width: size,
        height: size,
    }
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t) as u8
}
