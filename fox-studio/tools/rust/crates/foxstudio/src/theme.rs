//! Restrained dark and light themes, with an optional high-contrast palette.
use crate::settings::ThemeChoice;
use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Theme,
    ThemePreference, Visuals,
};
use std::sync::Arc;

/// Independent of dark/light/system preference, so either theme can use high contrast.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Contrast {
    #[default]
    Standard,
    High,
}

fn contrast_id() -> egui::Id {
    egui::Id::new("foxstudio_theme_contrast")
}

pub fn contrast(ctx: &egui::Context) -> Contrast {
    ctx.data(|data| data.get_temp::<Contrast>(contrast_id()).unwrap_or_default())
}

/// Status colours (stage state, log lines, badges), tuned for both themes.
#[derive(Clone, Copy)]
pub struct Palette {
    pub accent: Color32,
    /// Label and keyboard-focus colour when the background is the opaque accent.
    pub on_accent: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    pub info: Color32,
    pub muted: Color32,
    pub card: Color32,
    pub card_stroke: Color32,
    pub node: Color32,
}

pub fn palette(dark: bool) -> Palette {
    palette_with_contrast(dark, Contrast::Standard)
}

pub fn palette_with_contrast(dark: bool, contrast: Contrast) -> Palette {
    if contrast == Contrast::High {
        return if dark {
            Palette {
                accent: Color32::from_rgb(0xFF, 0xC7, 0x66),
                on_accent: Color32::BLACK,
                ok: Color32::from_rgb(0x7B, 0xE0, 0x99),
                warn: Color32::from_rgb(0xFF, 0xD4, 0x5E),
                err: Color32::from_rgb(0xFF, 0x9E, 0x9B),
                info: Color32::from_rgb(0x8B, 0xC2, 0xFF),
                muted: Color32::from_rgb(0xBE, 0xC4, 0xCC),
                card: Color32::from_gray(0x10),
                card_stroke: Color32::from_gray(0xA0),
                node: Color32::from_gray(0x18),
            }
        } else {
            Palette {
                accent: Color32::from_rgb(0x72, 0x38, 0x00),
                on_accent: Color32::WHITE,
                ok: Color32::from_rgb(0x12, 0x5E, 0x2B),
                warn: Color32::from_rgb(0x67, 0x3F, 0x00),
                err: Color32::from_rgb(0x93, 0x15, 0x1D),
                info: Color32::from_rgb(0x0C, 0x42, 0x8B),
                muted: Color32::from_rgb(0x41, 0x46, 0x4F),
                card: Color32::WHITE,
                card_stroke: Color32::from_gray(0x55),
                node: Color32::WHITE,
            }
        };
    }
    if dark {
        Palette {
            accent: Color32::from_rgb(0xE8, 0x9A, 0x3C),
            on_accent: Color32::from_rgb(0x1A, 0x1C, 0x20),
            ok: Color32::from_rgb(0x5C, 0xC0, 0x7A),
            warn: Color32::from_rgb(0xE6, 0xB4, 0x50),
            err: Color32::from_rgb(0xFF, 0x85, 0x83),
            info: Color32::from_rgb(0x6A, 0xA8, 0xF0),
            muted: Color32::from_rgb(0x95, 0x9C, 0xA5),
            card: Color32::from_rgb(0x22, 0x25, 0x2A),
            card_stroke: Color32::from_rgb(0x33, 0x37, 0x3E),
            node: Color32::from_rgb(0x2A, 0x2E, 0x34),
        }
    } else {
        Palette {
            accent: Color32::from_rgb(0x96, 0x52, 0x0F),
            on_accent: Color32::WHITE,
            ok: Color32::from_rgb(0x1A, 0x74, 0x39),
            warn: Color32::from_rgb(0x80, 0x50, 0x00),
            err: Color32::from_rgb(0xC6, 0x2A, 0x2A),
            info: Color32::from_rgb(0x1F, 0x5F, 0xBF),
            muted: Color32::from_rgb(0x5C, 0x64, 0x70),
            card: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            card_stroke: Color32::from_rgb(0xD8, 0xDC, 0xE2),
            node: Color32::from_rgb(0xF4, 0xF5, 0xF7),
        }
    }
}

pub fn pal(ui: &egui::Ui) -> Palette {
    palette_with_contrast(ui.visuals().dark_mode, contrast(ui.ctx()))
}

fn visuals(dark: bool, contrast: Contrast) -> Visuals {
    let p = palette_with_contrast(dark, contrast);
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let high = contrast == Contrast::High;
    let text = match (dark, high) {
        (true, true) => Color32::WHITE,
        (false, true) => Color32::BLACK,
        (true, false) => Color32::from_rgb(0xE0, 0xE4, 0xEB),
        (false, false) => Color32::from_rgb(0x17, 0x1B, 0x21),
    };
    if high {
        v.panel_fill = if dark { Color32::BLACK } else { Color32::WHITE };
        v.window_fill = p.card;
        v.extreme_bg_color = v.panel_fill;
        v.faint_bg_color = p.card;
        v.code_bg_color = v.panel_fill;
        v.text_edit_bg_color = Some(v.panel_fill);
    } else if dark {
        v.panel_fill = Color32::from_rgb(0x1A, 0x1C, 0x20);
        v.window_fill = Color32::from_rgb(0x20, 0x23, 0x28);
        v.extreme_bg_color = Color32::from_rgb(0x13, 0x15, 0x18);
        v.faint_bg_color = Color32::from_rgb(0x1F, 0x22, 0x26);
        v.code_bg_color = Color32::from_rgb(0x15, 0x17, 0x1A);
        v.text_edit_bg_color = Some(Color32::from_rgb(0x16, 0x18, 0x1C));
    } else {
        v.panel_fill = Color32::from_rgb(0xF0, 0xF1, 0xF3);
        v.window_fill = Color32::from_rgb(0xFA, 0xFA, 0xFB);
        v.extreme_bg_color = Color32::WHITE;
        v.faint_bg_color = Color32::from_rgb(0xF6, 0xF7, 0xF8);
        v.code_bg_color = p.node;
        v.text_edit_bg_color = Some(Color32::from_rgb(0xF6, 0xF7, 0xF9));
    }

    let border = match (dark, high) {
        (_, true) => p.card_stroke,
        (true, false) => Color32::from_rgb(0x70, 0x79, 0x84),
        (false, false) => Color32::from_rgb(0x7A, 0x81, 0x8C),
    };
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.bg_fill = p.node;
        w.weak_bg_fill = p.node;
        w.bg_stroke = Stroke::new(1.0, border);
        w.fg_stroke = Stroke::new(1.0, text);
        w.corner_radius = CornerRadius::same(5);
        w.expansion = 0.0;
    }
    // egui uses active visuals for keyboard focus as well as pressed buttons.
    // TextEdit uses selection.stroke; both receive the same visible outline.
    v.widgets.active.bg_stroke = Stroke::new(2.0, p.accent);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.accent);
    v.widgets.open.bg_stroke = Stroke::new(2.0, p.accent);
    v.weak_text_color = Some(p.muted);
    v.window_stroke = Stroke::new(1.0, p.card_stroke);
    v.selection.bg_fill = p.node;
    v.selection.stroke = Stroke::new(2.0, p.accent);
    v.text_cursor.stroke = Stroke::new(2.0, p.accent);
    v.hyperlink_color = p.info;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.err;
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    // key/value grids read better without stripes; tables ask for them explicitly
    v.striped = false;
    v
}

pub fn preference(choice: ThemeChoice) -> ThemePreference {
    match choice {
        ThemeChoice::System => ThemePreference::System,
        ThemeChoice::Dark => ThemePreference::Dark,
        ThemeChoice::Light => ThemePreference::Light,
    }
}

/// Apply theme choice + zoom. Called when settings change (cheap).
pub fn apply(ctx: &egui::Context, choice: ThemeChoice, scale: f32) {
    apply_with_contrast(ctx, choice, scale, Contrast::Standard);
}

/// Apply the same contrast choice to both system-theme variants.
pub fn apply_with_contrast(
    ctx: &egui::Context,
    choice: ThemeChoice,
    scale: f32,
    contrast: Contrast,
) {
    ctx.data_mut(|data| data.insert_temp(contrast_id(), contrast));
    ctx.set_visuals_of(Theme::Dark, visuals(true, contrast));
    ctx.set_visuals_of(Theme::Light, visuals(false, contrast));
    ctx.set_theme(preference(choice));
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        s.spacing.window_margin = Margin::same(12);
        s.spacing.interact_size.y = 24.0;
        s.text_styles = [
            (
                TextStyle::Heading,
                FontId::new(20.0, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(14.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(11.5, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(13.0, FontFamily::Monospace),
            ),
        ]
        .into();
    });
    if (ctx.zoom_factor() - scale).abs() > 1e-3 {
        ctx.set_zoom_factor(scale);
    }
}

/// Use Segoe UI and Consolas when Windows has them (read at run time from the system fonts folder; nothing is
/// bundled). egui's own fonts stay as fallbacks for missing glyphs.
pub fn install_fonts(ctx: &egui::Context, system_fonts: bool) {
    let mut fonts = egui::FontDefinitions::default();
    if system_fonts
        && let Some(dir) =
            std::env::var_os("WINDIR").map(|w| std::path::PathBuf::from(w).join("Fonts"))
    {
        let mut add = |key: &str, file: &str, family: FontFamily| {
            if let Ok(bytes) = std::fs::read(dir.join(file)) {
                fonts
                    .font_data
                    .insert(key.to_string(), Arc::new(egui::FontData::from_owned(bytes)));
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .insert(0, key.to_string());
            }
        };
        add("segoe-ui", "segoeui.ttf", FontFamily::Proportional);
        add("consolas", "consola.ttf", FontFamily::Monospace);
    }
    ctx.set_fonts(fonts);
}

/// A rounded card (sections of a view).
pub fn card(ui: &egui::Ui) -> egui::Frame {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.card_stroke))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(12))
}

/// A filled action needs its own text and focus colour, rather than a hard-coded black/white label.
pub fn primary_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    let p = pal(ui);
    ui.scope(|ui| {
        ui.visuals_mut().widgets.active.bg_stroke = Stroke::new(2.0, p.on_accent);
        ui.visuals_mut().widgets.hovered.bg_stroke = Stroke::new(1.0, p.on_accent);
        ui.add_enabled(
            enabled,
            egui::Button::new(egui::RichText::new(label).strong().color(p.on_accent))
                .fill(p.accent),
        )
    })
    .inner
}

/// small coloured pill (status badges in the top bar and tables)
pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    // A very light tint keeps small status text readable on the card and node backgrounds.
    let fill = if contrast(ui.ctx()) == Contrast::High {
        Color32::TRANSPARENT
    } else {
        color.gamma_multiply(0.04)
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, color))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).color(color).size(12.0))
        })
        .response
}

/// section title inside a card
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(egui::RichText::new(title).strong().size(15.0));
    ui.add_space(4.0);
}

/// procedural window icon: an accent rounded square with a stylised "F" (no image files in the binary)
pub fn icon() -> egui::IconData {
    let n = 64usize;
    let mut rgba = vec![0u8; n * n * 4];
    let acc = [0xE8u8, 0x9A, 0x3C];
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // rounded square, radius 12
            let r = 12.0;
            let cx = fx.clamp(r, n as f32 - r);
            let cy = fy.clamp(r, n as f32 - r);
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let a = (r + 0.5 - d).clamp(0.0, 1.0);
            let i = (y * n + x) * 4;
            // "F": stem 18..28 x 14..50, top bar 18..46 x 14..23, middle bar 18..40 x 29..37
            let in_f = (18..28).contains(&x) && (14..50).contains(&y)
                || (18..46).contains(&x) && (14..23).contains(&y)
                || (18..40).contains(&x) && (29..37).contains(&y);
            let c = if in_f { [0x1A, 0x1C, 0x20] } else { acc };
            rgba[i] = c[0];
            rgba[i + 1] = c[1];
            rgba[i + 2] = c[2];
            rgba[i + 3] = (a * 255.0) as u8;
        }
    }
    egui::IconData {
        rgba,
        width: n as u32,
        height: n as u32,
    }
}
