//! Read-only About and licence viewer for the notices in a portable release bundle.
use crate::theme;
use eframe::egui::{self, Color32, FontId, RichText, Stroke, TextFormat, text::LayoutJob};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_NOTICE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    Project,
    ThirdParty,
    Mit,
    Apache,
}

impl NoticeKind {
    fn index(self) -> usize {
        match self {
            Self::Project => 0,
            Self::ThirdParty => 1,
            Self::Mit => 2,
            Self::Apache => 3,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Project => "Project notice",
            Self::ThirdParty => "Third-party licences",
            Self::Mit => "MIT licence",
            Self::Apache => "Apache-2.0 licence",
        }
    }
}

/// Text and provenance stay together, including when a bundle is incomplete.
pub struct NoticeDocument {
    path: PathBuf,
    content: Result<String, String>,
}

impl NoticeDocument {
    fn load(path: PathBuf) -> Self {
        let content = read_notice(&path);
        Self { path, content }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn text(&self) -> Result<&str, &str> {
        self.content.as_deref().map_err(String::as_str)
    }
}

fn read_notice(path: &Path) -> Result<String, String> {
    let metadata = path
        .metadata()
        .map_err(|error| format!("Cannot inspect bundled notice: {error}"))?;
    if !metadata.is_file() {
        return Err("Bundled notice is not a regular file.".into());
    }
    if metadata.len() > MAX_NOTICE_BYTES {
        return Err("Bundled notice exceeds the 8 MiB display limit.".into());
    }
    let file = File::open(path).map_err(|error| format!("Cannot open bundled notice: {error}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_NOTICE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Cannot read bundled notice: {error}"))?;
    if bytes.len() as u64 > MAX_NOTICE_BYTES {
        return Err("Bundled notice exceeds the 8 MiB display limit.".into());
    }
    let text =
        String::from_utf8(bytes).map_err(|_| "Bundled notice must be valid UTF-8.".to_owned())?;
    // A UTF-8 BOM describes the encoding; it is not part of the notice text.
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned();
    if text.trim().is_empty() {
        return Err("Bundled notice is empty.".into());
    }
    Ok(text)
}

/// Load once when the application starts or first opens About, rather than on every frame.
pub struct AboutView {
    documents: [NoticeDocument; 4],
    selected: NoticeKind,
    search: String,
    copied: bool,
}

impl AboutView {
    /// Never searches the working directory, private repository or a Python installation.
    pub fn load_bundled() -> Self {
        match std::env::current_exe() {
            Ok(executable) => match executable.parent() {
                Some(directory) => Self::load_from_dir(directory),
                None => Self::unavailable("Cannot locate the executable's release folder.".into()),
            },
            Err(error) => Self::unavailable(format!("Cannot locate the executable: {error}")),
        }
    }

    /// Explicit directory for portable packages and isolated test fixtures.
    pub fn load_from_dir(directory: &Path) -> Self {
        Self {
            documents: [
                NoticeDocument::load(directory.join("NOTICE")),
                NoticeDocument::load(directory.join("THIRD_PARTY.txt")),
                NoticeDocument::load(directory.join("LICENSE-MIT")),
                NoticeDocument::load(directory.join("LICENSE-APACHE")),
            ],
            selected: NoticeKind::Project,
            search: String::new(),
            copied: false,
        }
    }

    fn unavailable(error: String) -> Self {
        Self {
            documents: [
                NoticeDocument {
                    path: "NOTICE".into(),
                    content: Err(error.clone()),
                },
                NoticeDocument {
                    path: "THIRD_PARTY.txt".into(),
                    content: Err(error.clone()),
                },
                NoticeDocument {
                    path: "LICENSE-MIT".into(),
                    content: Err(error.clone()),
                },
                NoticeDocument {
                    path: "LICENSE-APACHE".into(),
                    content: Err(error),
                },
            ],
            selected: NoticeKind::Project,
            search: String::new(),
            copied: false,
        }
    }

    pub fn document(&self, kind: NoticeKind) -> &NoticeDocument {
        &self.documents[kind.index()]
    }

    pub fn selected(&self) -> NoticeKind {
        self.selected
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        theme::card(ui).show(ui, |ui| {
            theme::section(ui, "About Fox Studio");
            ui.label(format!("Fox Studio {}", env!("CARGO_PKG_VERSION")));
            ui.label("Desktop editor for the Fox Engine tools.");
            ui.label(format!("Licensed under {}.", env!("CARGO_PKG_LICENSE")));
            ui.add_space(6.0);

            if notice_tabs(ui, &mut self.selected) {
                self.copied = false;
                self.search.clear();
            }
            let document = &self.documents[self.selected.index()];
            show_document(
                ui,
                document,
                self.selected,
                &mut self.search,
                &mut self.copied,
            );
        });
    }
}

fn notice_tabs(ui: &mut egui::Ui, selected: &mut NoticeKind) -> bool {
    let previous = *selected;
    ui.horizontal_wrapped(|ui| {
        for kind in [
            NoticeKind::Project,
            NoticeKind::ThirdParty,
            NoticeKind::Mit,
            NoticeKind::Apache,
        ] {
            ui.selectable_value(selected, kind, kind.label());
        }
    });
    previous != *selected
}

fn show_document(
    ui: &mut egui::Ui,
    document: &NoticeDocument,
    kind: NoticeKind,
    search: &mut String,
    copied: &mut bool,
) {
    let path = RichText::new(document.path.display().to_string())
        .monospace()
        .color(theme::pal(ui).muted);
    ui.add(egui::Label::new(path).wrap());
    let text = match document.text() {
        Ok(text) => text,
        Err(error) => {
            ui.colored_label(
                theme::pal(ui).err,
                format!("{} unavailable: {error}", kind.label()),
            );
            ui.label("Use a complete Fox Studio release bundle with NOTICE, THIRD_PARTY.txt, LICENSE-MIT and LICENSE-APACHE beside the executable, then reopen Fox Studio.");
            return;
        }
    };
    notice_controls(ui, text, search, copied);
    if *copied {
        ui.label("Full notice copied.");
    }
    if !search.is_empty() {
        let matches = text.match_indices(search.as_str()).count();
        ui.label(match matches {
            0 => "No matching text. The full notice is shown below.".into(),
            1 => "1 match. The full notice is shown below.".into(),
            count => format!("{count} matches. The full notice is shown below."),
        });
    }
    egui::ScrollArea::vertical()
        .id_salt(("foxstudio_bundled_notice", kind.index()))
        .max_height(300.0)
        .show(ui, |ui| {
            let job = notice_layout(
                text,
                search,
                FontId::proportional(14.0),
                ui.visuals().text_color(),
                theme::pal(ui).accent,
            );
            ui.add(egui::Label::new(job).selectable(true).wrap());
        });
}

fn notice_controls(ui: &mut egui::Ui, text: &str, search: &mut String, copied: &mut bool) {
    ui.horizontal_wrapped(|ui| {
        let label = ui.label("Find in notice");
        ui.add(
            egui::TextEdit::singleline(search)
                .id_salt("foxstudio_notice_search")
                .desired_width(220.0)
                .hint_text("Find text (case-sensitive)"),
        )
        .labelled_by(label.id);
        if ui.button("Clear search").clicked() {
            search.clear();
        }
        if ui.button("Copy full notice").clicked() {
            ui.ctx().copy_text(text.to_owned());
            *copied = true;
        }
    });
}

fn notice_layout(
    text: &str,
    search: &str,
    font_id: FontId,
    color: Color32,
    accent: Color32,
) -> LayoutJob {
    let normal = TextFormat {
        font_id,
        color,
        ..Default::default()
    };
    let highlighted = TextFormat {
        underline: Stroke::new(2.0, accent),
        ..normal.clone()
    };
    let mut job = LayoutJob::default();
    let mut end = 0;
    if !search.is_empty() {
        for (start, matched) in text.match_indices(search) {
            job.append(&text[end..start], 0.0, normal.clone());
            job.append(matched, 0.0, highlighted.clone());
            end = start + matched.len();
        }
    }
    job.append(&text[end..], 0.0, normal);
    job
}
