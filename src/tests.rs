use super::*;

fn project_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
    std::fs::create_dir(dir.path().join("target")).unwrap();
    std::fs::write(dir.path().join("target/artifact"), "build artifact").unwrap();
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    dir
}

fn scan_directory(
    path: &Path,
    read_only_check: impl Fn(&Path) -> io::Result<bool>,
) -> (Vec<ProjectDir>, Vec<Job>) {
    let (job_tx, job_rx) = crossbeam_channel::unbounded();
    let (result_tx, result_rx) = crossbeam_channel::unbounded();
    let args = AppArgs::parse_from(["cargo-clean-all"]);
    find_cargo_projects_task(
        Job::new(path.to_owned(), job_tx, None),
        &ProgressBar::hidden(),
        result_tx,
        &args,
        read_only_check,
    );
    (result_rx.try_iter().collect(), job_rx.try_iter().collect())
}

#[test]
fn read_only_project_is_not_reported_or_traversed() {
    let dir = project_fixture();
    let (projects, jobs) = scan_directory(dir.path(), |_| Ok(true));
    assert!(projects.is_empty());
    assert!(jobs.is_empty());
    assert!(dir.path().join("target/artifact").exists());
}

#[test]
fn writable_project_is_reported_and_traversed() {
    let dir = project_fixture();
    let (projects, jobs) = scan_directory(dir.path(), |_| Ok(false));
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].0, dir.path());
    assert!(projects[0].1);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].path, dir.path().join("nested"));
}

#[test]
fn read_only_target_mount_is_not_cleanable() {
    let dir = project_fixture();
    let target = dir.path().join("target");
    let (projects, jobs) = scan_directory(dir.path(), |path| Ok(path == target));
    assert_eq!(projects.len(), 1);
    assert!(!projects[0].1);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].path, dir.path().join("nested"));
}

#[test]
fn filesystem_check_failure_skips_directory() {
    let dir = project_fixture();
    let (projects, jobs) = scan_directory(dir.path(), |_| {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    });
    assert!(projects.is_empty());
    assert!(jobs.is_empty());
}

#[test]
fn target_filesystem_check_failure_is_not_cleanable() {
    let dir = project_fixture();
    let (projects, _) = scan_directory(dir.path(), |path| {
        if path == dir.path() {
            Ok(false)
        } else {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        }
    });
    assert_eq!(projects.len(), 1);
    assert!(!projects[0].1);
}

#[cfg(unix)]
#[test]
fn detects_writable_filesystem() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!filesystem_is_read_only(dir.path()).unwrap());
}

#[cfg(unix)]
#[test]
fn filesystem_check_reports_missing_path() {
    let dir = tempfile::tempdir().unwrap();
    let error = filesystem_is_read_only(&dir.path().join("missing")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

#[cfg(unix)]
#[test]
fn filesystem_check_uses_mount_flags_not_directory_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().metadata().unwrap().permissions();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = filesystem_is_read_only(dir.path());
    std::fs::set_permissions(dir.path(), original).unwrap();
    assert!(!result.unwrap());
}
