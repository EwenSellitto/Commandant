use std::path::PathBuf;

use crate::docker_paths::{common_docker_socket_candidates, first_matching_path};

pub(crate) fn detect_docker_socket() -> Option<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    detect_docker_socket_at(home)
}

pub(crate) fn detect_docker_socket_at(home: Option<PathBuf>) -> Option<String> {
    let home = home?;
    let candidates = common_docker_socket_candidates(&home, false);
    first_matching_path(candidates, is_socket_file).map(|path| path.display().to_string())
}

fn is_socket_file(path: &std::path::Path) -> bool {
    match std::fs::metadata(path) {
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt;
                metadata.file_type().is_socket()
            }
            #[cfg(not(unix))]
            {
                metadata.is_file()
            }
        }
        Err(_) => false,
    }
}
