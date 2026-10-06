//! `fox dict build`: the local file-name dictionary (foxcore::dict), written to --out; --compare reports how a given
//! (community) dictionary resolves the same entry hashes, locally only.
use foxcore::{dict, qar};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn build(
    context: &qar::Context,
    archives: &[String],
    exe: Option<&str>,
    out_path: &str,
    compare: Option<&str>,
    profile_path: Option<&Path>,
) {
    let paths: Vec<PathBuf> = archives.iter().map(PathBuf::from).collect();
    let mut protected: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    protected.extend(exe.map(Path::new));
    protected.extend(compare.map(Path::new));
    protected.extend(profile_path);
    crate::asset_paths::protect_inputs(&[PathBuf::from(out_path)], &protected)
        .unwrap_or_else(|error| crate::die(error));
    // per-archive summary lines on stderr, as before
    let mut progress = |_f: f32, text: &str| {
        if text.contains("tokens so far") {
            eprintln!("{text}");
        }
        true
    };
    let (found, st) = match dict::build_with_context(
        context,
        &paths,
        exe.map(std::path::Path::new),
        &mut progress,
    ) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("fox: dict build: {e}");
            std::process::exit(2)
        }
    };
    for (a, e) in &st.skipped {
        eprintln!("skip {}: {e}", a.display());
    }
    crate::output::write_atomic(Path::new(out_path), |file| {
        file.write_all(dict::to_text(&found).as_bytes())
    })
    .unwrap_or_else(|error| crate::die(format!("{out_path}: {error}")));
    println!(
        "dictionary: {} names resolve {} of {} entry hashes ({:.1}%) from {} tokens over {:.1} GB -> {out_path} ({:.0} s)",
        found.len(),
        st.resolved,
        st.wanted,
        100.0 * st.resolved as f64 / st.wanted.max(1) as f64,
        st.tokens,
        st.scanned_bytes as f64 / 1e9,
        st.seconds
    );
    if let Some(c) = compare {
        let text = std::fs::read_to_string(c)
            .unwrap_or_else(|error| crate::die(format!("comparison dictionary {c}: {error}")));
        let mut theirs: HashSet<u64> = HashSet::new();
        for l in text.lines() {
            let stem = l.trim().split('.').next().unwrap_or("");
            let h = qar::path_hash(stem) & dict::PATH_MASK;
            if st.wanted_hashes.contains(&h) {
                theirs.insert(h);
            }
        }
        let both = theirs.intersection(&st.resolved_hashes).count();
        println!(
            "comparison (local only): the given dictionary resolves {} of these entry hashes; ours {}; both {}; \
                      only theirs {}; only ours {}",
            theirs.len(),
            st.resolved,
            both,
            theirs.len() - both,
            st.resolved - both
        );
    }
}
