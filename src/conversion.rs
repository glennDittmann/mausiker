use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug)]
pub struct CompletedConversion {
    pub original: PathBuf,
    pub output: PathBuf,
}

pub fn output_path(library_root: &Path, source: &Path) -> Result<PathBuf, String> {
    if !source.starts_with(library_root) {
        return Err("source is outside the selected music library".to_owned());
    }
    Ok(source.with_extension("m4a"))
}

pub fn convert(library_root: &Path, source: &Path) -> Result<CompletedConversion, String> {
    if source
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("m4a"))
    {
        return Err("the source is already an M4A file".into());
    }
    let output = output_path(library_root, source)?;
    if output.exists() {
        return Err(format!("output already exists: {}", output.display()));
    }
    let parent = output
        .parent()
        .ok_or_else(|| "output path has no parent".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = output.with_file_name(format!(
        ".{}.partial.m4a",
        output
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("output")
    ));
    if temporary.exists() {
        return Err(format!(
            "temporary output already exists: {}",
            temporary.display()
        ));
    }

    let process = Command::new("ffmpeg")
        .args(["-n", "-i"])
        .arg(source)
        .args([
            "-map",
            "0:a?",
            "-map",
            "0:v?",
            "-map_metadata",
            "0",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-c:v",
            "copy",
        ])
        .arg(&temporary)
        .output()
        .map_err(|error| format!("could not run ffmpeg: {error}"))?;
    if !process.status.success() {
        let message = String::from_utf8_lossy(&process.stderr);
        return Err(format!(
            "ffmpeg failed: {}",
            message.lines().next().unwrap_or("unknown error")
        ));
    }
    verify_output(&temporary)?;
    fs::rename(&temporary, &output).map_err(|error| error.to_string())?;
    Ok(CompletedConversion {
        original: source.to_path_buf(),
        output,
    })
}

pub fn verify_output(path: &Path) -> Result<(), String> {
    if fs::metadata(path).map_err(|error| error.to_string())?.len() == 0 {
        return Err("FFmpeg produced an empty file".into());
    }
    lofty::read_from_path(path)
        .map(|_| ())
        .map_err(|error| format!("output metadata verification failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::output_path;
    use std::path::Path;

    #[test]
    fn creates_an_m4a_file_beside_its_source() {
        let root = Path::new("/music/library");
        assert_eq!(
            output_path(root, Path::new("/music/library/Rap/Artist/Album/01.flac")).unwrap(),
            Path::new("/music/library/Rap/Artist/Album/01.m4a")
        );
    }

    #[test]
    fn rejects_a_source_outside_the_library() {
        assert!(
            output_path(
                Path::new("/music/library"),
                Path::new("/music/elsewhere/01.flac")
            )
            .is_err()
        );
    }
}
