use std::path::{Path, PathBuf};

pub fn common_docker_socket_candidates(home: &Path, include_rd: bool) -> Vec<PathBuf> {
    let mut candidates = vec![
        home.join(".docker/run/docker.sock"),
        home.join(".colima/default/docker.sock"),
    ];

    if include_rd {
        candidates.push(home.join(".rd/docker.sock"));
    }

    candidates.push(home.join(".orbstack/run/docker.sock"));
    candidates.push(PathBuf::from("/var/run/docker.sock"));
    candidates
}

pub fn first_matching_path(
    paths: impl IntoIterator<Item = PathBuf>,
    matches: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    paths.into_iter().find(|path| matches(path))
}
