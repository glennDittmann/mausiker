mod browser;
mod conversion;
mod library;

use std::{env, path::PathBuf};

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let root = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(env::current_dir()?);
    ratatui::run(|terminal| browser::run(terminal, root))?;
    Ok(())
}
