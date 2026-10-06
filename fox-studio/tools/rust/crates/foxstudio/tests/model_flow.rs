//! Read-only synthetic model inspection and UI contracts; no window, GPU, build stage or install.
#[path = "common/model_fixture.rs"]
mod model_fixture;
use egui_kittest::{Harness, kittest::Queryable};
use foxstudio::preview::model;
use foxstudio::preview_view::{PreviewView, Sub};
use foxstudio::settings::{Settings, Tab};
use foxstudio::{AppOptions, FoxStudio};
use model_fixture::{ModelFixture, container};
use std::path::Path;
use std::time::{Duration, Instant};

#[test]
fn model_fixture_preserves_half_channels_material_identity_and_bind_bones() {
    let f = container();
    let m = model::decode(Path::new("fixture.fmdl"), &f.build()).unwrap();
    assert_eq!((m.info.meshes, m.info.vertices, m.info.triangles), (2, 8, 4));
    assert_eq!((m.meshes[0].material, m.meshes[1].material), (1, 0));
    assert_eq!(m.verts[0].nrm, [0.0, 0.0, 1.0]);
    assert_eq!(m.verts[4].nrm, [-1.0, 0.0, 0.0]); // geometric normals would point +Z
    assert_eq!(m.verts[2].uv, [1.0, 1.0]);
    assert_eq!(m.meshes[1].indices, 6..12);
    assert_eq!(m.info.bbox, ([-2.0, -1.0, 0.0], [2.0, 1.0, 0.1]));
    assert_eq!(m.bones[1].parent, Some(0));
    assert_eq!(m.bones[1].local, [2.2, 1.1, 0.0]);
    assert_eq!(m.bones[1].world, [1.2, 0.6, 0.0]);
    assert_eq!(m.bone_lines(Some(1)).len(), 14);
    assert!(m.notes.is_empty(), "{:?}", m.notes);
    let reference = f.meshes();
    assert_eq!(reference[1].0, m.verts[4..8].iter().map(|v| v.pos).collect::<Vec<_>>());
    assert_eq!(reference[1].1, vec![[0, 1, 2], [0, 2, 3]]);
}

#[test]
fn model_fixture_binds_full_path_hash_and_keeps_rectangular_atlas_mips() {
    let f = ModelFixture::new();
    let m = model::load_with(&f.model, &f.options()).unwrap();
    assert_eq!(m.materials.iter().filter(|m| m.image.is_some()).count(), 2);
    let red = m.materials[0].image.as_ref().unwrap();
    assert_eq!(
        red.iter().map(|i| (i.width, i.height)).collect::<Vec<_>>(),
        [(4, 2), (2, 1), (1, 1)]
    );
    assert_eq!(&red[0].rgba[..4], &[230, 35, 20, 255]);
    assert_eq!(&red[0].rgba[16..20], &[25, 55, 235, 255]);
    assert_eq!(m.materials[1].image.as_ref().unwrap()[0].width, 8);
    assert_ne!(m.materials[0].base_hash, m.materials[1].base_hash);
    assert_eq!(
        m.materials[0].path.as_ref().unwrap().file_name(),
        m.materials[1].path.as_ref().unwrap().file_name()
    );
    f.assert_unchanged();
}

#[test]
fn model_fixture_missing_textures_are_explicit_and_geometry_remains_inspectable() {
    let f = ModelFixture::new();
    let m = model::load(&f.model).unwrap();
    assert!(
        m.materials
            .iter()
            .all(|m| m.image.is_none() && m.status.starts_with("unresolved albedo"))
    );
    assert_eq!(m.info.triangles, 4);
    f.assert_unchanged();
}

#[test]
fn model_fixture_rejects_corrupt_geometry_and_unsupported_versions() {
    let mut f = container();
    f.section1[208..210].copy_from_slice(&99u16.to_le_bytes());
    assert!(
        model::decode(Path::new("bad.fmdl"), &f.build())
            .unwrap_err()
            .contains("outside vertex")
    );
    let mut f = container();
    f.head[4..8].copy_from_slice(&3.0f32.to_le_bytes());
    assert!(
        model::decode(Path::new("new.fmdl"), &f.build())
            .unwrap_err()
            .contains("unsupported FMDL version")
    );
    let mut f = container();
    f.section1[16..20].copy_from_slice(&f32::NAN.to_le_bytes());
    assert!(
        model::decode(Path::new("nan.fmdl"), &f.build())
            .unwrap_err()
            .contains("nonfinite")
    );
}

#[test]
fn model_fixture_unsupported_uv_normal_channels_are_labelled_fallbacks() {
    let mut f = container();
    f.blocks.get_mut(&11).unwrap().raw[5] = 99;
    f.blocks.get_mut(&11).unwrap().raw[9] = 99;
    let m = model::decode(Path::new("channels.fmdl"), &f.build()).unwrap();
    assert!(!m.meshes[0].has_uv);
    assert!(!m.meshes[0].source_normals);
    assert!(m.notes.iter().any(|n| n.contains("usage 8 format 99")));
    assert!(m.notes.iter().any(|n| n.contains("usage 2 format 99")));
    assert_eq!(m.verts[0].nrm, [0.0, 0.0, 1.0]);
}

#[test]
fn model_fixture_rejects_cyclic_and_out_of_range_bone_parents() {
    let mut f = container();
    f.blocks.get_mut(&0).unwrap().raw[2..4].copy_from_slice(&1u16.to_le_bytes());
    assert!(
        model::decode(Path::new("cycle.fmdl"), &f.build())
            .unwrap_err()
            .contains("cyclic")
    );
    f.blocks.get_mut(&0).unwrap().raw[2..4].copy_from_slice(&99u16.to_le_bytes());
    assert!(
        model::decode(Path::new("parent.fmdl"), &f.build())
            .unwrap_err()
            .contains("parent outside")
    );
}

#[test]
fn model_fixture_half_conversion_covers_sign_subnormal_and_nonfinite() {
    assert_eq!(model::half_to_f32(0x3c00), 1.0);
    assert_eq!(model::half_to_f32(0xbc00), -1.0);
    assert_eq!(model::half_to_f32(1), 2.0f32.powi(-24));
    assert!(model::half_to_f32(0x7c00).is_infinite());
    assert!(model::half_to_f32(0x7e00).is_nan());
}

fn wait(h: &mut Harness<'static, FoxStudio>, done: impl Fn(&FoxStudio) -> bool) {
    let start = Instant::now();
    loop {
        h.step();
        if done(h.state()) {
            h.run_steps(2);
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "model UI timeout");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn model_fixture_headless_ui_exposes_inspection_and_clears_errors_on_new_selection() {
    let f = ModelFixture::new();
    let s = Settings {
        repo_root: f.root.display().to_string(),
        cache_dir: f.cache.display().to_string(),
        last_tab: Tab::Previews,
        system_fonts: false,
        ..Default::default()
    };
    let mut h = Harness::builder().with_size([1440.0, 1000.0]).build_eframe(move |cc| {
        FoxStudio::new(
            &cc.egui_ctx,
            s,
            None,
            AppOptions {
                settings_path: None,
                allow_real_builds: false,
                probe_every: Duration::from_secs(60),
            },
        )
    });
    h.state_mut().previews.sub = Sub::Models;
    h.state_mut().settings.preview_model = f.model.display().to_string();
    wait(&mut h, |a| a.previews.model.as_ref().is_some_and(|m| m.is_ok()));
    h.get_by_label("Normal vectors").click();
    h.get_by_label("Bind bones").click();
    h.run_steps(2);
    assert!(h.state().previews.model_normals && h.state().previews.model_bones);
    assert!(
        h.state()
            .previews
            .model
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .materials
            .iter()
            .all(|m| m.image.is_some())
    );
    let ctx = h.ctx.clone();
    h.state_mut().settings.preview_model = f.broken.display().to_string();
    h.state_mut().previews.open_model(&ctx, f.broken.clone());
    assert!(h.state().previews.model.is_none());
    wait(&mut h, |a| a.previews.model.as_ref().is_some_and(|m| m.is_err()));
    h.state_mut().settings.preview_model = f.model.display().to_string();
    h.state_mut().previews.open_model(&ctx, f.model.clone());
    wait(&mut h, |a| a.previews.model.as_ref().is_some_and(|m| m.is_ok()));
    f.assert_unchanged();
}

#[test]
fn model_fixture_reload_and_reset_never_keep_displayed_old_model() {
    let f = ModelFixture::new();
    let mut v = PreviewView::default();
    let ctx = eframe::egui::Context::default();
    v.open_model_with(&ctx, f.model.clone(), f.options());
    let start = Instant::now();
    while v.model.is_none() {
        v.poll();
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
    v.open_model_with(&ctx, f.model.clone(), f.options());
    assert!(v.model.is_none());
    v.reset_models();
    assert!(v.model.is_none() && v.model_list.is_empty());
    f.assert_unchanged();
}

#[test]
fn model_budget_shares_images_across_materials_but_preserves_colour_space() {
    let f = ModelFixture::new();
    let mut source = container();
    let hash = foxcore::qar::path_hash(model_fixture::RED_PATH).to_le_bytes();
    source.blocks.get_mut(&21).unwrap().raw[8..16].copy_from_slice(&hash);
    std::fs::write(&f.model, source.build()).unwrap();
    let mut m = model::load_with(&f.model, &f.options()).unwrap();
    assert_eq!(m.unique_texture_materials().len(), 1);
    assert!(std::sync::Arc::ptr_eq(
        m.materials[0].image.as_ref().unwrap(),
        m.materials[1].image.as_ref().unwrap()
    ));
    m.materials[1].linear = true;
    assert_eq!(m.unique_texture_materials().len(), 2);
}

#[test]
fn model_budget_refuses_oversized_albedo_before_decode() {
    let f = ModelFixture::new();
    let red = f.cache.join("patch/Assets/tpp/mecha/fixture/Pictures/panel.png");
    let mut b = std::fs::read(&red).unwrap();
    b[16..20].copy_from_slice(&8192u32.to_be_bytes());
    std::fs::write(&red, b).unwrap();
    let m = model::load_with(&f.model, &f.options()).unwrap();
    assert!(m.materials[0].image.is_none());
    assert!(
        m.materials[0].status.contains("8192 x 2 exceeds"),
        "{}",
        m.materials[0].status
    );
    assert!(m.materials[1].image.is_some());
}
