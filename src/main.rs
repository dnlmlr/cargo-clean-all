use clap::Parser;
use colored::{Color, Colorize};
use crossbeam_channel::{SendError, Sender};
use indicatif::{ProgressBar, ProgressStyle};
use is_executable::is_executable;
use std::{
    fmt::Display,
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime},
};

#[derive(Debug, Parser)]
#[clap(author, version, about, bin_name = "cargo clean-all", long_about = None)]
struct AppArgs {
    /// The directory in which the projects will be searched
    #[arg(default_value_t  = String::from("."), value_name = "DIR")]
    root_dir: String,

    /// Don't ask for confirmation; Just clean all detected projects that are not excluded by other
    /// constraints
    #[arg(short = 'y', long = "yes")]
    yes: bool,

    /// Ignore projects with a target dir size smaller than the specified value. The size can be
    /// specified using binary prefixes like "10MB" for 10_000_000 bytes, or "1KiB" for 1_024 bytes
    #[arg(
        short = 's',
        long = "keep-size",
        value_name = "SIZE",
        default_value_t = 0,
        value_parser = parse_bytes_from_str
    )]
    keep_size: u64,

    /// Ignore projects that have been compiled in the last [DAYS] days. The last compilation time
    /// is infered by the last modified time of the contents of target directory.
    #[arg(
        short = 'd',
        long = "keep-days",
        value_name = "DAYS",
        default_value_t = 0
    )]
    keep_last_modified: u32,

    /// Just collect the cleanable projects and list the freeable space, but don't delete anything
    #[arg(long = "dry-run")]
    dry_run: bool,

    /// The number of threads to use for directory scanning. 0 automatically selects the number of
    /// threads
    #[arg(
        short = 't',
        long = "threads",
        value_name = "THREADS",
        default_value_t = 0
    )]
    number_of_threads: usize,

    /// Show access errors that occur while scanning. By default those errors are hidden
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,

    /// Use the interactive project selection. This will show a selection of all cleanable projects
    /// with the possibility to manually select or deselect
    #[arg(short = 'i', long = "interactive")]
    interactive: bool,

    /// Directories that should be ignored by default, including subdirectories. This will still
    /// detect the projects in those directories, but mark them to not be cleaned. To actually skip
    /// scanning directories, use --skip instead.
    /// The directories can be specified as absolute paths or relative to the workdir.
    #[arg(long = "ignore")]
    ignore: Vec<String>,

    /// Keeping compiled executables in release, debug and cross-compilation directories.
    /// Moves the executable to a new folder outside of target.
    #[arg(short = 'e', long = "keep-executable")]
    executable: bool,

    /// Directories that should be fully skipped during scanning, including subdirectories. This
    /// will speed up the scanning time by not doing any reads for the specified directories.
    /// The directories can be specified as absolute paths or relative to the workdir.
    #[arg(long = "skip")]
    skip: Vec<String>,

    /// Maximum depth of subdirectories that should be scanned looking for the **`target/`**. This will speed up the scanning
    /// The option is for target/ dir, NOT for the project dir
    /// 0 means no limit
    #[arg(long = "depth", default_value_t = 0)]
    depth: usize,
}

/// Wrap the bytefmt::parse function to return the error as an owned String
fn parse_bytes_from_str(byte_str: &str) -> Result<u64, String> {
    bytefmt::parse(byte_str).map_err(|e| e.to_string())
}

/// Try to get the canonicalized path and return the non canonicalized path if it doesn't work
fn canonicalize_or_not(p: impl AsRef<Path>) -> PathBuf {
    std::fs::canonicalize(p.as_ref()).unwrap_or_else(|_| p.as_ref().to_path_buf())
}

fn starts_with_canonicalized(a: impl AsRef<Path>, b: impl AsRef<Path>) -> bool {
    canonicalize_or_not(a).starts_with(canonicalize_or_not(b))
}

fn main() {
    // If the program is interrupted while in a dialog the cursor stays hidden. This makes sure
    // that the cursor is shown when interrupting the program
    ctrlc::set_handler(|| {
        let _ = dialoguer::console::Term::stdout().show_cursor();
        std::process::exit(1);
    })
    .unwrap();

    // Enable ANSI escape codes on window 10. This always returns `Ok(())`, so unwrap is fine
    #[cfg(windows)]
    colored::control::set_virtual_terminal(true).unwrap();

    let mut args = std::env::args();

    // When called using `cargo clean-all`, the argument `clean-all` is inserted. To fix the arg
    // alignment, one argument is dropped.
    if let Some("clean-all") = std::env::args().nth(1).as_deref() {
        args.next();
    }

    let args = AppArgs::parse_from(args);

    let scan_path = Path::new(&args.root_dir);

    let scan_progress = ProgressBar::new_spinner()
        .with_message(format!("Scanning for projects in {}", args.root_dir))
        .with_style(ProgressStyle::default_spinner().tick_strings(&[
            "[=---------]",
            "[-=--------]",
            "[--=-------]",
            "[---=------]",
            "[----=-----]",
            "[-----=----]",
            "[------=---]",
            "[-------=--]",
            "[--------=-]",
            "[---------=]",
            "[--------=-]",
            "[-------=--]",
            "[------=---]",
            "[-----=----]",
            "[----=-----]",
            "[---=------]",
            "[--=-------]",
            "[-=--------]",
            "[=---------]",
        ]));

    scan_progress.enable_steady_tick(Duration::from_millis(100));

    // Find project dirs and analyze them
    let mut projects: Vec<_> = find_cargo_projects(scan_path, args.number_of_threads, &args)
        .into_iter()
        .filter_map(|proj| proj.1.then(|| ProjectTargetAnalysis::analyze(&proj.0)))
        .collect();

    projects.sort_by_key(|proj| proj.size);

    // Determin what projects are selected by the restrictions
    let preselected_projects = projects
        .iter_mut()
        .map(|tgt| {
            let secs_elapsed = tgt
                .last_modified
                .elapsed()
                .unwrap_or_default()
                .as_secs_f32();
            let days_elapsed = secs_elapsed / (60.0 * 60.0 * 24.0);
            let ignored = args
                .ignore
                .iter()
                .any(|p| starts_with_canonicalized(&tgt.project_path, p));

            days_elapsed >= args.keep_last_modified as f32 && tgt.size > args.keep_size && !ignored
        })
        .collect::<Vec<_>>();

    scan_progress.finish_and_clear();

    if args.interactive {
        let Ok(Some(prompt)) = dialoguer::MultiSelect::new()
            .items(&projects)
            .with_prompt("Select projects to clean")
            .report(false)
            .defaults(&preselected_projects)
            .interact_opt()
        else {
            println!("Nothing selected");
            return;
        };

        for idx in prompt {
            projects[idx].selected_for_cleanup = true;
        }
    } else {
        for i in 0..preselected_projects.len() {
            projects[i].selected_for_cleanup = preselected_projects[i];
        }
    }

    let (selected, ignored): (Vec<_>, Vec<_>) = projects
        .into_iter()
        .partition(|proj| proj.selected_for_cleanup);

    let will_free_size: u64 = selected.iter().map(|it| it.size).sum();
    let ignored_free_size: u64 = ignored.iter().map(|it| it.size).sum();

    println!("Ignoring the following project directories:");
    ignored.iter().for_each(|p| println!("{}", p));

    println!("\nSelected the following project directories for cleaning:");
    selected.iter().for_each(|p| println!("{}", p));

    println!(
        "\nSelected {}/{} projects, cleaning will free: {}. Keeping: {}",
        selected.len(),
        selected.len() + ignored.len(),
        bytefmt::format(will_free_size).bold(),
        bytefmt::format(ignored_free_size)
    );

    if args.dry_run {
        println!("Dry run. Not doing any cleanup");
        return;
    }

    // Confirm cleanup if --yes is not present in the args
    if !args.yes {
        if !dialoguer::Confirm::new()
            .with_prompt("Clean the project directories shown above?")
            .wait_for_newline(true)
            .interact()
            .unwrap()
        {
            println!("Cleanup cancelled");
            return;
        }
    }

    println!("Starting cleanup...");

    let clean_progress = ProgressBar::new(selected.len() as u64)
        .with_message("Deleting target directories")
        .with_style(
            ProgressStyle::default_bar()
                .template("{msg} [{bar:20}] {pos:>3}/{len:3}")
                .unwrap()
                .progress_chars("=> "),
        );

    // Saves the executables in another folder before cleaning the target folder
    if args.executable {
        for project in selected.iter() {
            let project_target_path = &project.project_path.join("target");
            let project_executables_path = project.project_path.join("executables");

            let target_rd = match project_target_path.read_dir() {
                Ok(it) => it,
                Err(e) => {
                    args.verbose
                        .then(|| eprintln!("Error reading target dir of: '{}'  {}", project, e));
                    continue;
                }
            };

            let target_rd = target_rd
                .filter_map(|it| it.ok())
                .filter_map(|it| it.file_type().is_ok_and(|t| t.is_dir()).then(|| it.path()));

            for target_subdir in target_rd {
                let files = match target_subdir.read_dir() {
                    Ok(it) => it,
                    Err(e) => {
                        args.verbose.then(|| {
                            eprintln!("Error reading target dir of: '{}'  {}", project, e)
                        });
                        continue;
                    }
                };

                let files = files
                    .filter_map(|it| it.ok())
                    .filter_map(|it| it.file_type().is_ok_and(|t| t.is_file()).then(|| it.path()));

                for exe_file_path in files.filter(|file| is_executable(file)) {
                    let new_exe_file_path = project_executables_path
                        .join(target_subdir.file_name().expect("Path Error"))
                        .join(exe_file_path.file_name().expect("Path Error"));

                    if let Err(e) =
                        std::fs::create_dir_all(new_exe_file_path.parent().expect("Path Error"))
                    {
                        eprintln!(
                            "Error createing executable dir: '{}'  {}",
                            new_exe_file_path.parent().expect("Path Error").display(),
                            e
                        );
                        continue;
                    }

                    if let Err(e) = std::fs::rename(exe_file_path, &new_exe_file_path) {
                        eprintln!(
                            "Error moving executable: '{}'  {}",
                            new_exe_file_path.display(),
                            e
                        );
                        continue;
                    }
                }
            }
        }
    }

    let failed_cleanups = selected
        .iter()
        .filter_map(|tgt| {
            clean_progress.inc(1);
            remove_dir_all::remove_dir_all(&tgt.project_path.join("target"))
                .err()
                .map(|e| (tgt.clone(), e))
        })
        .collect::<Vec<_>>();

    clean_progress.finish();

    // The current leftover size calculation assumes that a failed deletion didn't delete anything.
    // This will not be true in most cases as a recursive deletion might delet stuff before failing.
    let mut leftover_size = 0;
    for (tgt, e) in &failed_cleanups {
        leftover_size += tgt.size;
        println!("Failed to clean {}", pretty_format_path(&tgt.project_path));
        println!("Error: {}", e);
    }

    println!(
        "\nAll projects cleaned. Reclaimed {} of disk space",
        bytefmt::format(will_free_size - leftover_size).bold()
    );
}

/// Job for the threaded project finder. First the path to be searched, second the sender to create
/// new jobs for recursively searching the dirs
struct Job {
    path: PathBuf,
    sender: Sender<Job>,
    depth: Option<usize>,
}

impl Job {
    pub fn new(path: PathBuf, sender: Sender<Job>, depth: Option<usize>) -> Self {
        Self {
            path,
            sender,
            depth,
        }
    }

    pub fn explore_recursive(&self, path: PathBuf) -> Result<(), SendError<Self>> {
        self.sender.send(Job {
            path,
            sender: self.sender.clone(),
            depth: self.depth.map(|d| d - 1),
        })
    }
}

/// Directory of the project and bool that is true if the target directory exists
struct ProjectDir(PathBuf, bool);

/// Recursively scan the given path for cargo projects using the specified number of threads.
///
/// When the number of threads is 0, use as many threads as virtual CPU cores.
fn find_cargo_projects(path: &Path, mut num_threads: usize, args: &AppArgs) -> Vec<ProjectDir> {
    if num_threads == 0 {
        num_threads = num_cpus::get();
    }
    let depth = (args.depth > 0).then(|| args.depth);

    thread::scope(|scope| {
        {
            let (job_tx, job_rx) = crossbeam_channel::unbounded::<Job>();
            let (result_tx, result_rx) = crossbeam_channel::unbounded::<ProjectDir>();

            (0..num_threads)
                .map(|_| (job_rx.clone(), result_tx.clone()))
                .for_each(|(job_rx, result_tx)| {
                    scope.spawn(move || {
                        job_rx
                            .into_iter()
                            .for_each(|job| find_cargo_projects_task(job, result_tx.clone(), &args))
                    });
                });

            job_tx
                .clone()
                .send(Job::new(path.to_path_buf(), job_tx, depth))
                .unwrap();

            result_rx
        }
        .into_iter()
        .collect()
    })
}

/// Scan the given directory and report to the results Sender if the directory contains a
/// Cargo.toml . Detected subdirectories should be queued as a new job in with the job_sender.
///
/// This function is supposed to be called by the threadpool in find_cargo_projects
fn find_cargo_projects_task(job: Job, results: Sender<ProjectDir>, args: &AppArgs) {
    if let Some(0) = job.depth {
        return;
    }
    let mut has_target = false;

    let read_dir = match job.path.read_dir() {
        Ok(it) => it,
        Err(e) => {
            args.verbose
                .then(|| eprintln!("Error reading directory: '{}'  {}", job.path.display(), e));
            return;
        }
    };
    let (dirs, files): (Vec<_>, Vec<_>) = read_dir
        .filter_map(|it| it.ok())
        .partition(|it| it.file_type().is_ok_and(|t| t.is_dir()));
    let dirs = dirs.iter().map(|it| it.path());
    let has_cargo_toml = files
        .iter()
        .any(|it| it.file_name().to_string_lossy() == "Cargo.toml");
    // Iterate through the subdirectories of path, ignoring entries that caused errors
    for it in dirs {
        if args.skip.iter().any(|p| starts_with_canonicalized(&it, p)) {
            continue;
        }

        let filename = it.file_name().unwrap_or_default().to_string_lossy();
        match filename.as_ref() {
            // No need to search .git directories for cargo projects. Also skip .cargo directories
            // as there shouldn't be any target dirs in there. Even if there are valid target dirs,
            // they should probably not be deleted. See issue #2 (https://github.com/dnlmlr/cargo-clean-all/issues/2)
            ".git" | ".cargo" => (),
            "target" if has_cargo_toml => has_target = true,
            // For directories queue a new job to search it with the threadpool
            _ => job.explore_recursive(it.to_path_buf()).unwrap(),
        }
    }

    // If path contains a Cargo.toml, it is a project directory
    if has_cargo_toml {
        results.send(ProjectDir(job.path, has_target)).unwrap();
    }
}

#[derive(Clone, Debug)]
struct ProjectTargetAnalysis {
    /// The path of the project without the `target` directory suffix
    project_path: PathBuf,
    /// The size in bytes that the target directory takes up
    size: u64,
    /// The timestamp of the last recently modified file in the target directory
    last_modified: SystemTime,
    /// Indicate that this target directory should be cleaned
    selected_for_cleanup: bool,
}

impl ProjectTargetAnalysis {
    /// Analyze a given project directories target directory
    pub fn analyze(path: &Path) -> Self {
        let (size, last_modified) = Self::recursive_scan_target(&path.join("target"));
        Self {
            project_path: path.to_owned(),
            size,
            last_modified,
            selected_for_cleanup: false,
        }
    }

    // Recursively sum up the file sizes and find the last modified timestamp
    fn recursive_scan_target<T: AsRef<Path>>(path: T) -> (u64, SystemTime) {
        let path = path.as_ref();

        let default = (0, SystemTime::UNIX_EPOCH);

        if !path.exists() {
            return default;
        }

        match (path.is_file(), path.metadata()) {
            (true, Ok(md)) => (md.len(), md.modified().unwrap_or(default.1)),
            _ => path
                .read_dir()
                .map(|rd| {
                    rd.filter_map(|it| it.ok().map(|it| it.path()))
                        .map(Self::recursive_scan_target)
                        .fold(default, |a, b| (a.0 + b.0, a.1.max(b.1)))
                })
                .unwrap_or(default),
        }
    }
}

/// Remove the `\\?\` prefix from canonicalized windows paths and replace all `\` path separators
/// with `/`. This could make paths non-copyable in some special cases but those paths are mainly
/// intended for identifying the projects, so this is fine.
fn pretty_format_path(p: &Path) -> String {
    p.display()
        .to_string()
        .replace("\\\\?\\", "")
        .replace('\\', "/")
}

impl Display for ProjectTargetAnalysis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let project_name = self
            .project_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let path = pretty_format_path(&canonicalize_or_not(&self.project_path));

        let last_modified: chrono::DateTime<chrono::Local> = self.last_modified.into();
        write!(
            f,
            "{}: {} ({}), {}",
            project_name.bold().color(Color::Green),
            bytefmt::format(self.size),
            last_modified.format("%Y-%m-%d %H:%M"),
            path,
        )
    }
}
