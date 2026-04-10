use std::fs;
use std::path::{Path, PathBuf};

use docker_compose_spec::DockerCompose;

use crate::error::{Error, Result};

pub fn parse_str(yaml: &str) -> Result<docker_compose_spec::DockerCompose> {
    Ok(yaml.parse::<DockerCompose>()?)
}

pub fn parse_file(path: impl AsRef<Path>) -> Result<(DockerCompose, PathBuf)> {
    let path = path.as_ref();
    let contents = fs::read_to_string(path).map_err(|source| Error::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;

    let compose = parse_str(&contents)?;
    let base_dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    Ok((compose, base_dir))
}
