//! Linux-only filesystem inspection for a configured stdio executable.
//!
//! This is a non-executing advisory safety gate, NOT a trust certificate:
//! checking metadata can race with later replacement of a path. A future
//! runner must use an appropriately hardened execution boundary, revalidate
//! immediately before launch, and still require explicit human activation,
//! an authorized one-shot tool route, bounded IO, and an isolated environment.

use chatarium_core::tool::StdioToolProviderConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioExecutableInspectionError {
    UnsupportedPlatform,
    PathUnavailable,
    SymlinkComponent,
    NonDirectoryAncestor,
    WritableAncestor,
    NotRegularFile,
    NotExecutable,
    WritableExecutable,
}

/// Check basic Linux executable path properties without ever opening it for
/// execution, spawning processes, or modifying persistent state.
///
/// The path must have no symlink components, parent directories must not be
/// group/world writable, and the target must be a regular executable not
/// writable by group/world. This is intentionally conservative; it may reject
/// otherwise useful paths such as executables under /tmp or symlinked aliases.
pub fn inspect_stdio_executable(
    config: &StdioToolProviderConfig,
) -> Result<(), StdioExecutableInspectionError> {
    #[cfg(target_os = "linux")]
    {
        inspect_linux(config.executable())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = config;
        Err(StdioExecutableInspectionError::UnsupportedPlatform)
    }
}

#[cfg(target_os = "linux")]
fn inspect_linux(path: &str) -> Result<(), StdioExecutableInspectionError> {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Component, Path, PathBuf};

    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(StdioExecutableInspectionError::PathUnavailable);
    }

    let components = path.components().collect::<Vec<_>>();
    if components.len() < 2 || components.first() != Some(&Component::RootDir) {
        return Err(StdioExecutableInspectionError::PathUnavailable);
    }

    let mut current = PathBuf::from("/");
    for (index, component) in components.iter().enumerate().skip(1) {
        let Component::Normal(name) = component else {
            return Err(StdioExecutableInspectionError::PathUnavailable);
        };
        current.push(name);

        // symlink_metadata intentionally does not follow symlinks.
        let metadata = fs::symlink_metadata(&current)
            .map_err(|_| StdioExecutableInspectionError::PathUnavailable)?;
        if metadata.file_type().is_symlink() {
            return Err(StdioExecutableInspectionError::SymlinkComponent);
        }

        let is_last = index == components.len() - 1;
        let mode = metadata.permissions().mode();
        if !is_last {
            if !metadata.is_dir() {
                return Err(StdioExecutableInspectionError::NonDirectoryAncestor);
            }
            if mode & 0o022 != 0 {
                return Err(StdioExecutableInspectionError::WritableAncestor);
            }
        } else {
            if !metadata.is_file() {
                return Err(StdioExecutableInspectionError::NotRegularFile);
            }
            if mode & 0o111 == 0 {
                return Err(StdioExecutableInspectionError::NotExecutable);
            }
            if mode & 0o022 != 0 {
                return Err(StdioExecutableInspectionError::WritableExecutable);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatarium_core::tool::ToolOperationName;

    fn config(path: &str) -> StdioToolProviderConfig {
        StdioToolProviderConfig::new(
            path,
            Vec::new(),
            vec![ToolOperationName::new("inspect").unwrap()],
        )
        .unwrap()
    }

    #[test]
    fn does_not_activate_missing_executable() {
        let checked = inspect_stdio_executable(&config(
            "/chatarium-nonexistent-stdio-executable-928474/not-here",
        ));
        #[cfg(target_os = "linux")]
        assert_eq!(checked, Err(StdioExecutableInspectionError::PathUnavailable));
        #[cfg(not(target_os = "linux"))]
        assert_eq!(checked, Err(StdioExecutableInspectionError::UnsupportedPlatform));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_symlink_path_component_even_if_target_is_executable() {
        // /proc/self/exe is a kernel-managed symlink, not an executable path
        // that should be allowed into an external stdio provider config.
        assert_eq!(
            inspect_stdio_executable(&config("/proc/self/exe")),
            Err(StdioExecutableInspectionError::SymlinkComponent)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inspects_running_executable_without_launching_it() {
        // No trust assertion: this only checks whether the file layout meets
        // the conservative metadata policy on this particular CI host.
        let path = std::env::current_exe().unwrap();
        let config = config(path.to_str().unwrap());
        let inspection = inspect_stdio_executable(&config);
        assert!(
            inspection.is_ok()
                || matches!(
                    inspection,
                    Err(StdioExecutableInspectionError::WritableAncestor)
                        | Err(StdioExecutableInspectionError::SymlinkComponent)
                )
        );
    }
}
