//! Archive metadata belongs to the user's installation, not the tool package.
use foxcore::{
    qar::Context,
    runtime_data::{PackOrder, RuntimeProfile},
};
use std::path::{Path, PathBuf};

/// The explicitly selected archive keys and package ordering rules for a command.
pub struct RuntimeAccess {
    pub archive: Context,
    pub order: PackOrder,
    pub profile: Option<RuntimeProfile>,
}

impl RuntimeAccess {
    pub fn load(path: Option<&Path>, game: Option<&Path>) -> Result<Self, String> {
        if let Some(path) = path {
            let profile = RuntimeProfile::load(path).map_err(|error| error.to_string())?;
            if let Some(game) = game {
                profile
                    .validate_for_game(game)
                    .map_err(|error| error.to_string())?;
            }
            return Ok(Self {
                archive: profile.qar_context(),
                order: profile.pack_order().clone(),
                profile: Some(profile),
            });
        }

        #[cfg(feature = "internal-pipeline")]
        return Ok(Self {
            archive: Context::internal(),
            order: PackOrder::internal(),
            profile: None,
        });

        #[cfg(not(feature = "internal-pipeline"))]
        Err("archive metadata is not configured; run fox setup --game GAME --out PROFILE, then pass --runtime-profile PROFILE".into())
    }
}

/// Consume our global option before a subcommand parses its own arguments.
pub fn take_profile_option(arguments: &mut Vec<String>) -> Result<Option<PathBuf>, String> {
    let mut selected = None;
    while let Some(index) = arguments
        .iter()
        .position(|argument| argument == "--runtime-profile")
    {
        if selected.is_some() {
            return Err("--runtime-profile can only be supplied once".into());
        }
        let value = arguments
            .get(index + 1)
            .filter(|value| !value.is_empty() && !value.starts_with("--"))
            .ok_or("--runtime-profile needs a file path")?;
        selected = Some(PathBuf::from(value));
        arguments.drain(index..=index + 1);
    }
    Ok(selected.or_else(|| std::env::var_os("FOX_RUNTIME_PROFILE").map(PathBuf::from)))
}

pub fn setup(arguments: &[String]) -> Result<(), String> {
    let game = crate::arg_value(arguments, "--game").ok_or("setup requires --game GAME")?;
    let output = crate::arg_value(arguments, "--out").ok_or("setup requires --out PROFILE")?;
    let game_root = Path::new(&game)
        .canonicalize()
        .map_err(|error| format!("{game}: {error}"))?;
    let output_path = crate::asset_paths::resolved_output(Path::new(&output))?;
    if output_path.starts_with(&game_root) {
        return Err("save the runtime profile outside the game installation".into());
    }
    let mut progress = |fraction: f32, message: &str| {
        eprintln!("{:3.0}% {message}", fraction * 100.0);
        true
    };
    let profile = RuntimeProfile::learn(Path::new(&game), &mut progress)
        .map_err(|error| error.to_string())?;
    profile
        .save(Path::new(&output))
        .map_err(|error| error.to_string())?;
    println!("Saved installation metadata to {output}");
    Ok(())
}
