//! Component contrast, keyboard and portable-notice tests on synthetic data only.
//! The ignored screenshot test requires a coordinator allocation and a software adapter.
#[path = "../src/about.rs"]
mod about;

use about::{AboutView, NoticeKind};
use eframe::egui::{self, Color32, Key, Stroke, Theme};
use egui_kittest::{Harness, kittest::Queryable};
use foxstudio::{settings::ThemeChoice, theme};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const PROJECT: &str =
    "Synthetic Fox Studio notice\r\nMIT OR Apache-2.0.\r\nCredits: café, 日本語.\r\n";
const THIRD_PARTY: &str = "Synthetic dependency notices\nfixture-crate 1.0\nSynthetic licence: retain this notice.\nSecond notice: café.\n";
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct NoticeFixture {
    root: PathBuf,
}

impl NoticeFixture {
    fn empty() -> Self {
        let root = std::env::temp_dir().join(format!(
            "foxstudio_f_notices_{}_{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).expect("new isolated notice fixture");
        Self { root }
    }

    fn complete() -> Self {
        let fixture = Self::empty();
        for (name, text) in [
            ("NOTICE", PROJECT),
            ("THIRD_PARTY.txt", THIRD_PARTY),
            (
                "LICENSE-MIT",
                "Synthetic MIT document for the component test.\n",
            ),
            (
                "LICENSE-APACHE",
                "Synthetic Apache document for the component test.\n",
            ),
        ] {
            std::fs::write(fixture.root.join(name), text).unwrap();
        }
        fixture
    }
}

impl Drop for NoticeFixture {
    fn drop(&mut self) {
        let target = self.root.canonicalize().expect("fixture still exists");
        let parent = std::env::temp_dir()
            .canonicalize()
            .expect("resolved temp directory");
        assert!(
            target.starts_with(parent)
                && target
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("foxstudio_f_notices_")
        );
        std::fs::remove_dir_all(target).expect("remove only our isolated fixture");
    }
}

fn luminance(color: Color32) -> f64 {
    let linear = |channel: u8| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
}

fn contrast_ratio(first: Color32, second: Color32) -> f64 {
    let a = luminance(first);
    let b = luminance(second);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// egui colours are premultiplied sRGBA; match its source-over gamma blending.
fn over(foreground: Color32, background: Color32) -> Color32 {
    let channel = |front: u8, back: u8| {
        (u16::from(front) + (u16::from(back) * (255 - u16::from(foreground.a())) + 127) / 255)
            .min(255) as u8
    };
    Color32::from_rgb(
        channel(foreground.r(), background.r()),
        channel(foreground.g(), background.g()),
        channel(foreground.b(), background.b()),
    )
}

fn choices() -> [(ThemeChoice, theme::Contrast); 4] {
    [
        (ThemeChoice::Dark, theme::Contrast::Standard),
        (ThemeChoice::Light, theme::Contrast::Standard),
        (ThemeChoice::Dark, theme::Contrast::High),
        (ThemeChoice::Light, theme::Contrast::High),
    ]
}

#[test]
fn text_status_and_control_contrast_in_all_four_themes() {
    for (choice, contrast) in choices() {
        let dark = choice == ThemeChoice::Dark;
        let ctx = egui::Context::default();
        theme::apply_with_contrast(&ctx, choice, 1.0, contrast);
        let style = ctx.style_of(if dark { Theme::Dark } else { Theme::Light });
        let v = &style.visuals;
        let p = theme::palette_with_contrast(dark, contrast);
        let target = if contrast == theme::Contrast::High {
            7.0
        } else {
            4.5
        };
        let backgrounds = [
            v.panel_fill,
            v.window_fill,
            p.card,
            p.node,
            v.extreme_bg_color,
            v.faint_bg_color,
            v.code_bg_color,
            v.text_edit_bg_color.unwrap(),
            v.selection.bg_fill,
            v.widgets.inactive.weak_bg_fill,
            v.widgets.hovered.weak_bg_fill,
            v.widgets.active.weak_bg_fill,
        ];
        let foregrounds = [
            v.text_color(),
            v.weak_text_color(),
            p.accent,
            p.ok,
            p.warn,
            p.err,
            p.info,
            p.muted,
        ];
        let mut minimum = f64::INFINITY;
        for foreground in foregrounds {
            assert_eq!(
                foreground.a(),
                255,
                "text contrast needs an opaque foreground"
            );
            for background in backgrounds {
                assert_eq!(background.a(), 255);
                let ratio = contrast_ratio(foreground, background);
                minimum = minimum.min(ratio);
                assert!(
                    ratio >= target,
                    "{choice:?}/{contrast:?}: {foreground:?} on {background:?} = {ratio:.3}, needs {target}"
                );
            }
        }
        for w in [&v.widgets.inactive, &v.widgets.hovered, &v.widgets.active] {
            assert!(
                contrast_ratio(w.bg_stroke.color, w.bg_fill) >= 3.0,
                "control boundary in {choice:?}/{contrast:?}"
            );
            assert!(contrast_ratio(w.fg_stroke.color, w.bg_fill) >= target);
        }
        assert_eq!(v.widgets.active.bg_stroke, v.selection.stroke);
        assert!(v.selection.stroke.width >= 2.0);
        eprintln!("{choice:?}/{contrast:?} minimum text/status contrast: {minimum:.6}:1");
    }
}

fn rectangle_with_stroke(
    shapes: &[egui::epaint::ClippedShape],
    color: Color32,
) -> &egui::epaint::RectShape {
    shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect)
                if rect.stroke.color == color
                    && rect.corner_radius == egui::CornerRadius::same(10) =>
            {
                Some(rect)
            }
            _ => None,
        })
        .expect("the actual rendered pill frame")
}

#[test]
fn small_status_pills_keep_contrast_after_actual_frame_tint() {
    for (choice, contrast) in choices() {
        let p = theme::palette_with_contrast(choice == ThemeChoice::Dark, contrast);
        for color in [p.accent, p.ok, p.warn, p.err, p.info] {
            for backdrop in [p.card, p.node] {
                let mut h = Harness::new_ui(move |ui| {
                    theme::apply_with_contrast(ui.ctx(), choice, 1.0, contrast);
                    egui::Frame::new().fill(backdrop).show(ui, |ui| {
                        theme::pill(ui, "Status", color);
                    });
                });
                h.run_steps(2);
                let frame = rectangle_with_stroke(&h.output().shapes, color);
                let ratio = contrast_ratio(color, over(frame.fill, backdrop));
                let target = if contrast == theme::Contrast::High {
                    7.0
                } else {
                    4.5
                };
                assert!(
                    ratio >= target,
                    "actual pill {choice:?}/{contrast:?}: {ratio:.6} < {target}"
                );
            }
        }
    }
}

#[test]
fn primary_action_text_and_keyboard_focus_are_readable_on_the_fill() {
    for (choice, contrast) in choices() {
        let mut h = Harness::builder().build_ui_state(
            move |ui, activated: &mut bool| {
                theme::apply_with_contrast(ui.ctx(), choice, 1.0, contrast);
                *activated |= theme::primary_button(ui, "Primary action", true).clicked();
            },
            false,
        );
        h.run_steps(2);
        let p = theme::palette_with_contrast(choice == ThemeChoice::Dark, contrast);
        let target = if contrast == theme::Contrast::High {
            7.0
        } else {
            4.5
        };
        assert!(contrast_ratio(p.on_accent, p.accent) >= target);
        h.key_press(Key::Tab);
        h.run_steps(2);
        assert!(h.output().shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Rect(rect) if rect.fill == p.accent && rect.stroke.color == p.on_accent && rect.stroke.width >= 2.0)),
            "focused action needs a painted outline visible on its own fill");
        h.key_press(Key::Enter);
        h.run_steps(2);
        assert!(
            *h.state(),
            "keyboard must activate the actual filled action"
        );
    }
}

#[derive(Default)]
struct FocusFixture {
    activated: bool,
    button_focused: bool,
    edit_focused: bool,
    focus_stroke: Stroke,
    text: String,
}

#[test]
fn tab_enter_and_shift_tab_keep_a_visible_keyboard_focus() {
    for (choice, contrast) in choices() {
        let mut h = Harness::builder().build_ui_state(
            move |ui, state: &mut FocusFixture| {
                theme::apply_with_contrast(ui.ctx(), choice, 1.0, contrast);
                let button = ui.button("Activate fixture");
                state.activated |= button.clicked();
                state.button_focused = button.has_focus();
                state.focus_stroke = ui.style().interact(&button).bg_stroke;
                let label = ui.label("Fixture text");
                let edit = ui
                    .text_edit_singleline(&mut state.text)
                    .labelled_by(label.id);
                state.edit_focused = edit.has_focus();
            },
            FocusFixture::default(),
        );
        h.run_steps(2);
        h.key_press(Key::Tab);
        h.run_steps(2);
        assert!(h.state().button_focused, "Tab must focus the first button");
        let style = h.ctx.style_of(if choice == ThemeChoice::Dark {
            Theme::Dark
        } else {
            Theme::Light
        });
        assert!(h.state().focus_stroke.width >= 2.0);
        assert!(contrast_ratio(h.state().focus_stroke.color, style.visuals.panel_fill) >= 3.0);
        h.key_press(Key::Enter);
        h.run_steps(2);
        assert!(
            h.state().activated,
            "Enter must activate the focused button"
        );
        h.key_press(Key::Tab);
        h.run_steps(2);
        assert!(h.state().edit_focused, "Tab must reach the text field");
        let focus = style.visuals.selection.stroke;
        let background = style.visuals.text_edit_bg_color.unwrap();
        assert!(
            h.output().shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Rect(rect) if rect.fill == background && rect.stroke == focus)),
            "focused text field must paint the visible selection outline"
        );
        h.event(egui::Event::Text("typed by keyboard".into()));
        h.run_steps(2);
        assert_eq!(h.state().text, "typed by keyboard");
        h.key_press_modifiers(egui::Modifiers::SHIFT, Key::Tab);
        h.run_steps(2);
        assert!(
            h.state().button_focused,
            "Shift+Tab must return to the button"
        );
    }
}

fn about_harness(
    directory: &Path,
    choice: ThemeChoice,
    contrast: theme::Contrast,
) -> Harness<'static, AboutView> {
    Harness::builder().with_size([900.0, 650.0]).build_ui_state(
        move |ui, view| {
            theme::apply_with_contrast(ui.ctx(), choice, 1.0, contrast);
            view.show(ui);
        },
        AboutView::load_from_dir(directory),
    )
}

#[test]
fn bundled_notices_load_exact_unicode_text_once_and_do_not_write_files() {
    let f = NoticeFixture::complete();
    let mut h = about_harness(&f.root, ThemeChoice::Dark, theme::Contrast::High);
    h.run_steps(2);
    assert_eq!(h.state().document(NoticeKind::Project).text(), Ok(PROJECT));
    assert_eq!(
        h.state().document(NoticeKind::ThirdParty).text(),
        Ok(THIRD_PARTY)
    );
    for label in [
        "Project notice",
        "Third-party licences",
        "MIT licence",
        "Apache-2.0 licence",
    ] {
        h.get_by_label(label).click();
        h.run_steps(2);
    }
    assert_eq!(
        std::fs::read_to_string(f.root.join("NOTICE")).unwrap(),
        PROJECT
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("THIRD_PARTY.txt")).unwrap(),
        THIRD_PARTY
    );
    std::fs::write(f.root.join("NOTICE"), "Changed fixture, reload required.").unwrap();
    assert_eq!(h.state().document(NoticeKind::Project).text(), Ok(PROJECT));
    assert_eq!(
        AboutView::load_from_dir(&f.root)
            .document(NoticeKind::Project)
            .text(),
        Ok("Changed fixture, reload required.")
    );
}

#[test]
fn notice_tabs_search_and_full_copy_work_from_keyboard() {
    let f = NoticeFixture::complete();
    let mut h = about_harness(&f.root, ThemeChoice::Light, theme::Contrast::High);
    h.get_by_label("Third-party licences").focus();
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().selected(), NoticeKind::ThirdParty);
    h.get_by_label("Find in notice").focus();
    h.run_steps(2);
    h.event(egui::Event::Text("café".into()));
    h.run_steps(2);
    h.get_by_label("1 match. The full notice is shown below.");
    h.get_by_label("Copy full notice").focus();
    h.run_steps(2);
    // Harness::step processes each queued event as a separate frame. Inspect
    // the copy command on key-down before key-up replaces the last output.
    h.key_down(Key::Enter);
    h.step();
    assert!(
        h.output()
            .platform_output
            .commands
            .iter()
            .any(|command| matches!(command,
        egui::OutputCommand::CopyText(text) if text == THIRD_PARTY)),
        "copy must include the whole unmodified licence, not only the search match"
    );
    h.key_up(Key::Enter);
    h.run_steps(2);
    h.get_by_label("Full notice copied.");
    h.get_by_label("Clear search").click();
    h.run_steps(2);
    h.get_by_label("MIT licence").click();
    h.run_steps(2);
    assert_eq!(h.state().selected(), NoticeKind::Mit);
    assert_eq!(
        h.state().document(NoticeKind::Mit).text(),
        Ok("Synthetic MIT document for the component test.\n")
    );
}

#[test]
fn missing_or_invalid_notices_show_packaging_errors_independently() {
    let f = NoticeFixture::empty();
    let mut h = about_harness(&f.root, ThemeChoice::Dark, theme::Contrast::Standard);
    h.run_steps(2);
    assert!(h.state().document(NoticeKind::Project).text().is_err());
    h.get_by_label("Use a complete Fox Studio release bundle with NOTICE, THIRD_PARTY.txt, LICENSE-MIT and LICENSE-APACHE beside the executable, then reopen Fox Studio.");
    std::fs::write(f.root.join("NOTICE"), PROJECT).unwrap();
    std::fs::write(f.root.join("THIRD_PARTY.txt"), [0xFF, 0xFE]).unwrap();
    let mut h = about_harness(&f.root, ThemeChoice::Light, theme::Contrast::Standard);
    assert_eq!(h.state().document(NoticeKind::Project).text(), Ok(PROJECT));
    assert_eq!(
        h.state().document(NoticeKind::ThirdParty).text(),
        Err("Bundled notice must be valid UTF-8.")
    );
    h.get_by_label("Third-party licences").click();
    h.run_steps(2);
    h.get_by_label("Third-party licences unavailable: Bundled notice must be valid UTF-8.");
}

#[test]
fn empty_oversized_and_nonfile_notices_are_not_silently_accepted() {
    let f = NoticeFixture::empty();
    std::fs::write(f.root.join("NOTICE"), " \r\n\t").unwrap();
    let oversized = std::fs::File::create(f.root.join("THIRD_PARTY.txt")).unwrap();
    oversized.set_len(8 * 1024 * 1024 + 1).unwrap();
    drop(oversized);
    std::fs::create_dir(f.root.join("LICENSE-MIT")).unwrap();
    let view = AboutView::load_from_dir(&f.root);
    assert_eq!(
        view.document(NoticeKind::Project).text(),
        Err("Bundled notice is empty.")
    );
    assert_eq!(
        view.document(NoticeKind::ThirdParty).text(),
        Err("Bundled notice exceeds the 8 MiB display limit.")
    );
    assert!(view.document(NoticeKind::Mit).text().is_err());
}

#[test]
fn utf8_bom_is_removed_without_changing_notice_content() {
    let f = NoticeFixture::complete();
    std::fs::write(
        f.root.join("THIRD_PARTY.txt"),
        format!("\u{feff}{THIRD_PARTY}"),
    )
    .unwrap();
    assert_eq!(
        AboutView::load_from_dir(&f.root)
            .document(NoticeKind::ThirdParty)
            .text(),
        Ok(THIRD_PARTY)
    );
}

#[test]
fn runtime_notices_are_relative_to_executable_not_working_directory() {
    let view = AboutView::load_bundled();
    let executable = std::env::current_exe().unwrap();
    assert_eq!(
        view.document(NoticeKind::Project).path(),
        executable.parent().unwrap().join("NOTICE")
    );
    assert_eq!(
        view.document(NoticeKind::ThirdParty).path(),
        executable.parent().unwrap().join("THIRD_PARTY.txt")
    );
}

#[test]
fn standard_apply_restores_standard_contrast_and_system_variants() {
    let ctx = egui::Context::default();
    theme::apply_with_contrast(&ctx, ThemeChoice::System, 1.5, theme::Contrast::High);
    // egui applies a requested zoom at the next UI pass.
    let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
    assert_eq!(theme::contrast(&ctx), theme::Contrast::High);
    assert_eq!(ctx.zoom_factor(), 1.5);
    for variant in [Theme::Dark, Theme::Light] {
        assert!(ctx.style_of(variant).visuals.widgets.active.bg_stroke.width >= 2.0);
    }
    theme::apply(&ctx, ThemeChoice::System, 1.0);
    // egui applies a requested zoom at the next UI pass.
    let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
    assert_eq!(theme::contrast(&ctx), theme::Contrast::Standard);
    assert_eq!(ctx.zoom_factor(), 1.0);
    assert_eq!(
        theme::palette(true).accent,
        theme::palette_with_contrast(true, theme::Contrast::Standard).accent
    );
}

#[test]
fn original_procedural_icon_is_legible_and_has_transparent_corners() {
    let icon = theme::icon();
    assert_eq!((icon.width, icon.height), (64, 64));
    assert_eq!(icon.rgba.len(), 64 * 64 * 4);
    let pixel = |x: usize, y: usize| {
        let index = (y * 64 + x) * 4;
        Color32::from_rgba_unmultiplied(
            icon.rgba[index],
            icon.rgba[index + 1],
            icon.rgba[index + 2],
            icon.rgba[index + 3],
        )
    };
    for (x, y) in [(0, 0), (63, 0), (0, 63), (63, 63)] {
        assert_eq!(pixel(x, y).a(), 0);
    }
    assert_eq!(pixel(20, 45).a(), 255);
    assert_eq!(pixel(43, 18), pixel(20, 45));
    assert_eq!(pixel(35, 33), pixel(20, 45));
    assert!(contrast_ratio(pixel(20, 45), pixel(43, 45)) >= 7.0);
}

#[test]
#[ignore = "requires coordinator CPU offscreen allocation and exclusive P cache access"]
fn screenshots_four_themes_about_and_keyboard_focus() {
    assert_eq!(
        std::env::var("VK_ICD_FILENAMES").as_deref(),
        Ok("/usr/share/vulkan/icd.d/lvp_icd.json"),
        "pin the Framework software adapter; never select a hardware GPU"
    );
    let output = std::env::var_os("FOX_STUDIO_POLISH_OUT")
        .map(PathBuf::from)
        .expect("isolated screenshot output directory");
    std::fs::create_dir_all(&output).unwrap();
    let f = NoticeFixture::complete();
    for (choice, contrast) in choices() {
        let mut h = Harness::builder()
            .with_size([900.0, 650.0])
            .wgpu_setup(software_renderer_setup())
            .build_ui_state(
                move |ui, view: &mut AboutView| {
                    theme::apply_with_contrast(ui.ctx(), choice, 1.0, contrast);
                    let p = theme::pal(ui);
                    ui.heading("Appearance and bundled licences");
                    ui.horizontal_wrapped(|ui| {
                        for (label, color) in [
                            ("Ready", p.ok),
                            ("Warning", p.warn),
                            ("Failed", p.err),
                            ("Information", p.info),
                        ] {
                            theme::pill(ui, label, color);
                        }
                        theme::primary_button(ui, "Primary action", true);
                    });
                    view.show(ui);
                },
                AboutView::load_from_dir(&f.root),
            );
        h.get_by_label("Third-party licences").click();
        h.run_steps(2);
        h.get_by_label("Find in notice").focus();
        h.run_steps(2);
        h.event(egui::Event::Text("café".into()));
        h.run_steps(2);
        h.get_by_label("Copy full notice").focus();
        h.run_steps(2);
        let image = h.render().expect("bounded software-rendered About image");
        let name = format!("about_{choice:?}_{contrast:?}.png").to_lowercase();
        image.save(output.join(&name)).unwrap();
        eprintln!("Saved {name}: {}x{}", image.width(), image.height());
    }
}

fn software_renderer_setup() -> eframe::egui_wgpu::WgpuSetup {
    use eframe::egui_wgpu::{self, wgpu};
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = wgpu::Backends::VULKAN;
    let descriptor = setup.device_descriptor.clone();
    setup.device_descriptor = std::sync::Arc::new(move |adapter| {
        let info = adapter.get_info();
        assert_eq!(info.backend, wgpu::Backend::Vulkan);
        assert_eq!(
            info.device_type,
            wgpu::DeviceType::Cpu,
            "reject a hardware adapter before requesting a device"
        );
        assert!(
            info.name.to_lowercase().contains("llvmpipe"),
            "expected the allocated Framework software adapter"
        );
        eprintln!(
            "F About proof adapter: {} / {:?} / {:?}",
            info.name, info.device_type, info.backend
        );
        descriptor(adapter)
    });
    egui_wgpu::WgpuSetup::CreateNew(setup)
}
