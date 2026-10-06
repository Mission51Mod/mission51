//! Native location creation and editing. App navigation and build execution stay with the app/runner.
use crate::projects::{self, Project};
use eframe::egui;
use foxpipe::project::Spec;
use foxproject::ProjectDocument;
use std::path::Path;
use toml_edit::{DocumentMut, Item, Table, TableLike, value};

#[derive(Clone, Debug, PartialEq)]
pub struct LocationForm {
    pub name: String,
    pub title: String,
    pub code: String,
    pub id: i64,
    pub grid: usize,
    pub biome: String,
    pub lake_y: Option<f64>,
}

impl Default for LocationForm {
    fn default() -> Self {
        Self {
            name: "newland".into(),
            title: "My location".into(),
            code: "newland".into(),
            id: foxpipe::project::validate::suggested_location_id(),
            grid: 2048,
            biome: "mafr".into(),
            lake_y: None,
        }
    }
}

impl LocationForm {
    fn from_spec(spec: &Spec) -> Self {
        let file = spec.file();
        Self {
            name: file.project.name.clone(),
            title: file.project.title.clone(),
            code: file.location.code.clone(),
            id: file.location.id,
            grid: file.location.grid,
            biome: file.biome.source.clone(),
            lake_y: file.terrain.lake_y,
        }
    }

    /// Patch authored tables without serializing a flattened resolved spec.
    pub fn apply_to(&self, original: &str) -> Result<String, String> {
        self.patch(original, None)
    }

    fn patch(&self, original: &str, saved: Option<&Self>) -> Result<String, String> {
        if self.lake_y.is_some_and(|height| !height.is_finite()) {
            return Err("water level must be finite".into());
        }
        let grid = i64::try_from(self.grid).map_err(|_| "grid size is too large")?;
        let mut document: DocumentMut = original
            .parse()
            .map_err(|error: toml_edit::TomlError| error.to_string())?;
        if saved.is_none_or(|saved| self.name != saved.name) {
            update(
                section(&mut document, "project")?,
                "name",
                value(&self.name),
            );
        }
        if saved.is_none_or(|saved| self.title != saved.title) {
            update(
                section(&mut document, "project")?,
                "title",
                value(&self.title),
            );
        }
        if saved.is_none_or(|saved| self.code != saved.code) {
            update(
                section(&mut document, "location")?,
                "code",
                value(&self.code),
            );
        }
        if saved.is_none_or(|saved| self.id != saved.id) {
            update(section(&mut document, "location")?, "id", value(self.id));
        }
        if saved.is_none_or(|saved| self.grid != saved.grid) {
            update(section(&mut document, "location")?, "grid", value(grid));
        }
        if saved.is_none_or(|saved| self.biome != saved.biome) {
            update(
                section(&mut document, "biome")?,
                "source",
                value(&self.biome),
            );
        }
        if saved.is_none_or(|saved| self.lake_y != saved.lake_y) {
            let terrain = section(&mut document, "terrain")?;
            match self.lake_y {
                Some(height) => {
                    update(terrain, "lake_y", value(height));
                }
                None => {
                    terrain.remove("lake_y");
                }
            }
        }
        let text = document.to_string();
        // Keep the authored line-ending convention when new fields are inserted.
        if original.contains("\r\n") && !original.replace("\r\n", "").contains('\n') {
            Ok(text.replace("\r\n", "\n").replace('\n', "\r\n"))
        } else {
            Ok(text)
        }
    }

    pub fn starter_text(&self) -> Result<String, String> {
        self.apply_to(
            r#"format = 1
[project]
name = "newland"
[location]
code = "newland"
id = 2
[build]
pinned = [{ path = "project.toml", owner = "location spec" }]
[[stage]]
name = "project.validate"
cmd = ["fox", "project", "check", "project.toml", "--json"]
owner = "project"
mem_gb = 0.1
"#,
        )
    }

    /// Create a real M3 document with a native validation action; no repository checkout is needed.
    pub fn create(&self, folder: &Path) -> Result<Project, String> {
        let text = self.starter_text()?;
        // Validate before creating a folder. A new standalone spec has no relative external templates/includes.
        let validation_root = existing_parent(folder)?;
        let origin = std::path::absolute(folder)
            .map_err(|error| error.to_string())?
            .join("project.toml");
        Spec::from_toml_str_in(&validation_root, &text, &origin, &Default::default())
            .map_err(|error| error.0)?;
        if folder.join(projects::M3_FILE_NAME).exists() || folder.join(projects::FILE_NAME).exists()
        {
            return Err(format!(
                "{} already holds a project; open it instead",
                folder.display()
            ));
        }
        std::fs::create_dir_all(folder)
            .map_err(|error| format!("{}: {error}", folder.display()))?;
        let document = ProjectDocument::create_in(folder, Path::new("project.toml"), &text)?;
        projects::load_in(document.repo_root(), document.path())
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.label("The bundled editor validates project settings and packages existing assets. Terrain, navigation, vegetation and mission generation are not available in this release.");
        egui::Grid::new("location_form")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                let label = ui.label("Project name");
                ui.text_edit_singleline(&mut self.name)
                    .labelled_by(label.id)
                    .on_hover_text("2 to 16 lowercase letters, digits or underscores");
                ui.end_row();
                let label = ui.label("Title");
                ui.text_edit_singleline(&mut self.title)
                    .labelled_by(label.id);
                ui.end_row();
                let label = ui.label("Location code");
                ui.text_edit_singleline(&mut self.code)
                    .labelled_by(label.id)
                    .on_hover_text("3 to 8 lowercase letters or digits, starting with a letter");
                ui.end_row();
                let label = ui.label("Location id");
                ui.add(egui::DragValue::new(&mut self.id).range(1..=65535))
                    .labelled_by(label.id);
                ui.end_row();
                ui.label("Grid size");
                egui::ComboBox::from_id_salt("location_grid")
                    .selected_text(self.grid.to_string())
                    .show_ui(ui, |ui| {
                        for grid in [2048, 2560, 3072, 3584, 4096] {
                            ui.selectable_value(&mut self.grid, grid, grid.to_string());
                        }
                    });
                ui.end_row();
                ui.label("Source biome");
                egui::ComboBox::from_id_salt("location_biome")
                    .selected_text(&self.biome)
                    .show_ui(ui, |ui| {
                        for biome in ["mafr", "afgh"] {
                            ui.selectable_value(&mut self.biome, biome.to_owned(), biome);
                        }
                    });
                ui.end_row();
                ui.label("Water level");
                ui.horizontal(|ui| {
                    let mut enabled = self.lake_y.is_some();
                    if ui.checkbox(&mut enabled, "Enabled").changed() {
                        self.lake_y = enabled.then_some(0.0);
                    }
                    if let Some(height) = &mut self.lake_y {
                        ui.add(egui::DragValue::new(height).speed(0.1).suffix(" m"));
                    }
                });
                ui.end_row();
            });
    }
}

pub struct LocationEditor {
    document: ProjectDocument,
    saved_form: LocationForm,
    pub form: LocationForm,
    pub error: Option<String>,
}

impl LocationEditor {
    pub fn open_in(root: &Path, file: &Path) -> Result<Self, String> {
        let document = ProjectDocument::open_in(root, file)?;
        let form = LocationForm::from_spec(document.spec());
        Ok(Self {
            document,
            saved_form: form.clone(),
            form,
            error: None,
        })
    }

    pub fn has_changes(&self) -> bool {
        self.form != self.saved_form
    }
    pub fn path(&self) -> &Path {
        self.document.path()
    }

    pub fn save(&mut self) -> Result<Project, String> {
        let text = self
            .form
            .patch(self.document.text(), Some(&self.saved_form))?;
        let spec = self.document.validate(&text)?;
        if self.form.lake_y != self.saved_form.lake_y
            && spec.file().terrain.lake_y != self.form.lake_y
        {
            return Err("Water level is inherited; edit the base project to disable it".into());
        }
        self.document.save(&text)?;
        self.form = LocationForm::from_spec(self.document.spec());
        self.saved_form = self.form.clone();
        self.error = None;
        projects::load_in(self.document.repo_root(), self.document.path()).map_err(|error| {
            format!(
                "Location settings were saved, but project metadata could not be refreshed: {error}"
            )
        })
    }

    /// Returns the refreshed project after a successful save; the app updates its selected project/preview.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<Project> {
        ui.heading("Location settings");
        self.form.ui(ui);
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if ui
            .add_enabled(self.has_changes(), egui::Button::new("Save location"))
            .clicked()
        {
            match self.save() {
                Ok(project) => return Some(project),
                Err(error) => self.error = Some(error),
            }
        }
        None
    }
}

fn section<'a>(document: &'a mut DocumentMut, name: &str) -> Result<&'a mut dyn TableLike, String> {
    document
        .as_table_mut()
        .entry(name)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or_else(|| format!("{name}: expected a TOML table"))
}

fn update(table: &mut dyn TableLike, name: &str, mut replacement: Item) {
    // Preserve the whitespace and inline comment attached to the edited value.
    if let Some(previous) = table.get(name).and_then(Item::as_value)
        && let Some(value) = replacement.as_value_mut()
    {
        *value.decor_mut() = previous.decor().clone();
    }
    table.insert(name, replacement);
}

fn existing_parent(folder: &Path) -> Result<std::path::PathBuf, String> {
    if folder.as_os_str().is_empty() {
        return Err("choose a project folder".into());
    }
    let mut parent = std::path::absolute(folder).map_err(|error| error.to_string())?;
    while !parent.exists() {
        if !parent.pop() {
            return Err("project folder has no existing parent".into());
        }
    }
    if !parent.is_dir() {
        return Err(format!("{} is not a directory", parent.display()));
    }
    Ok(parent)
}
