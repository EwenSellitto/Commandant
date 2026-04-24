use std::path::{Path, PathBuf};

pub const SYSTEM_DOCKER_SOCKET: &str = "/var/run/docker.sock";

#[cfg(target_os = "macos")]
pub fn docker_socket_candidates(home: Option<&Path>) -> Vec<PathBuf> {
    macos_docker_socket_candidates(home)
}

#[cfg(target_os = "linux")]
pub fn docker_socket_candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let xdg_runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    linux_docker_socket_candidates(home, xdg_runtime_dir.as_deref(), linux_uid())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn docker_socket_candidates(_home: Option<&Path>) -> Vec<PathBuf> {
    fallback_docker_socket_candidates()
}

pub fn detect_docker_socket(home: Option<&Path>) -> Option<PathBuf> {
    docker_socket_candidates(home)
        .into_iter()
        .find(|path| is_socket_file(path))
}

pub fn configure_docker_host() -> Option<PathBuf> {
    if std::env::var_os("DOCKER_HOST").is_some() {
        return None;
    }

    let home = std::env::var_os("HOME").map(PathBuf::from);
    let socket = detect_docker_socket(home.as_deref())?;

    // SAFETY: This helper is only intended for early process setup before creating the
    // Docker client. Call sites must ensure there is no concurrent environment access while
    // mutating DOCKER_HOST.
    unsafe {
        std::env::set_var("DOCKER_HOST", format!("unix://{}", socket.display()));
    }

    Some(socket)
}

pub fn docker_socket_mount_source() -> PathBuf {
    PathBuf::from(SYSTEM_DOCKER_SOCKET)
}

pub fn macos_docker_socket_candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = home {
        push_unique(&mut candidates, home.join(".docker/run/docker.sock"));
        push_unique(&mut candidates, home.join(".colima/default/docker.sock"));
        push_unique(&mut candidates, home.join(".rd/docker.sock"));
        push_unique(&mut candidates, home.join(".orbstack/run/docker.sock"));
    }
    push_unique(&mut candidates, PathBuf::from(SYSTEM_DOCKER_SOCKET));
    candidates
}

pub fn linux_docker_socket_candidates(
    home: Option<&Path>,
    xdg_runtime_dir: Option<&Path>,
    uid: Option<u32>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(xdg_runtime_dir) = xdg_runtime_dir {
        push_unique(&mut candidates, xdg_runtime_dir.join("docker.sock"));
    }
    if let Some(uid) = uid {
        push_unique(
            &mut candidates,
            PathBuf::from(format!("/run/user/{uid}/docker.sock")),
        );
    }
    if let Some(home) = home {
        push_unique(&mut candidates, home.join(".docker/desktop/docker.sock"));
        push_unique(&mut candidates, home.join(".docker/run/docker.sock"));
    }
    push_unique(&mut candidates, PathBuf::from(SYSTEM_DOCKER_SOCKET));
    candidates
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn fallback_docker_socket_candidates() -> Vec<PathBuf> {
    vec![PathBuf::from(SYSTEM_DOCKER_SOCKET)]
}

fn push_unique(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !paths.contains(&candidate) {
        paths.push(candidate);
    }
}

fn is_socket_file(path: &Path) -> bool {
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

#[cfg(target_os = "linux")]
fn linux_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;

    std::fs::metadata("/proc/self")
        .ok()
        .map(|metadata| metadata.uid())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        SYSTEM_DOCKER_SOCKET, docker_socket_mount_source, linux_docker_socket_candidates,
        macos_docker_socket_candidates,
    };

    #[test]
    fn macos_candidates_include_desktop_alternatives_and_system_socket() {
        let home = Path::new("/Users/tester");

        assert_eq!(
            macos_docker_socket_candidates(Some(home)),
            vec![
                PathBuf::from("/Users/tester/.docker/run/docker.sock"),
                PathBuf::from("/Users/tester/.colima/default/docker.sock"),
                PathBuf::from("/Users/tester/.rd/docker.sock"),
                PathBuf::from("/Users/tester/.orbstack/run/docker.sock"),
                PathBuf::from(SYSTEM_DOCKER_SOCKET),
            ]
        );
    }

    #[test]
    fn linux_candidates_include_rootless_desktop_and_system_socket() {
        let home = Path::new("/home/tester");
        let xdg_runtime_dir = Path::new("/tmp/runtime-tester");

        assert_eq!(
            linux_docker_socket_candidates(Some(home), Some(xdg_runtime_dir), Some(1000)),
            vec![
                PathBuf::from("/tmp/runtime-tester/docker.sock"),
                PathBuf::from("/run/user/1000/docker.sock"),
                PathBuf::from("/home/tester/.docker/desktop/docker.sock"),
                PathBuf::from("/home/tester/.docker/run/docker.sock"),
                PathBuf::from(SYSTEM_DOCKER_SOCKET),
            ]
        );
    }

    #[test]
    fn linux_candidates_deduplicate_same_rootless_socket() {
        let shared_runtime_dir = Path::new("/run/user/1000");

        assert_eq!(
            linux_docker_socket_candidates(None, Some(shared_runtime_dir), Some(1000)),
            vec![
                PathBuf::from("/run/user/1000/docker.sock"),
                PathBuf::from(SYSTEM_DOCKER_SOCKET),
            ]
        );
    }

    #[test]
    fn docker_socket_mount_source_uses_system_socket() {
        assert_eq!(
            docker_socket_mount_source(),
            PathBuf::from(SYSTEM_DOCKER_SOCKET)
        );
    }
}
