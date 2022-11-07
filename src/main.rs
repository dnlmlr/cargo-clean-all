use clap::Parser;
use colored::Colorize;
use crossbeam_channel::Sender;
use indicatif::{ProgressBar, ProgressStyle};
use std::{
    fmt::Display,
    path::{Path, PathBuf},
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
    /// specified using binary prefixes like "10MB" for 10_000_000 bytes, or "1KiB" for 1_000 bytes
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

    /// The number of threads to use for directory scaning. 0 automatically selects the number of
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

    /// Directories that should be ignored by default, including subdirectories
    #[arg(long = "ignore")]
    ignore: Vec<String>,
}

/// Wrap the bytefmt::parse function to return the error as an owned String
fn parse_bytes_from_str(byte_str: &str) -> Result<u64, String> {
    bytefmt::parse(byte_str).map_err(|e| e.to_string())
}

fn starts_with_canonicalized(a: impl AsRef<Path>, b: impl AsRef<Path>) -> bool {
    std::fs::canonicalize(a)
        .unwrap()
        .starts_with(std::fs::canonicalize(b).unwrap())
}

fn main() {
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
        .with_message("Scaning for projects")
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
    let mut projects: Vec<_> = find_cargo_projects(scan_path, args.number_of_threads, args.verbose)
        .into_iter()
        .filter_map(|proj| proj.1.then(|| ProjectTargetAnalysis::analyze(&proj.0)))
        .collect();

    projects.sort_by_key(|proj| proj.size);

    // Determin what projects are selected by the restrictions
    let preselected_projects = projects
        .iter()
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

    let Ok(Some(prompt)) = dialoguer::MultiSelect::new()
        .items(&projects)
        .with_prompt("Select projects to clean")
        .report(false)
        .defaults(&preselected_projects)
        .interact_opt() else {
            println!("Nothing selected");
            return;
        };

    for idx in prompt {
        projects[idx].selected_for_cleanup = true;
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

    selected.iter().for_each(|tgt| {
        clean_progress.inc(1);
        remove_dir_all::remove_dir_all(&tgt.project_path.join("target")).unwrap();
    });

    clean_progress.finish();

    println!(
        "All projects cleaned. Reclaimed {} of disk space",
        bytefmt::format(will_free_size).bold()
    );
}

/// Job for the threaded project finder. First the path to be searched, second the sender to create
/// new jobs for recursively searching the dirs
struct Job(PathBuf, Sender<Job>);

/// Directory of the project and bool that is true if the target directory exists
struct ProjectDir(PathBuf, bool);

/// Recursively scan the given path for cargo projects using the specified number of threads.
///
/// When the number of threads is 0, use as many threads as virtual CPU cores.
fn find_cargo_projects(path: &Path, mut num_threads: usize, verbose: bool) -> Vec<ProjectDir> {
    if num_threads == 0 {
        num_threads = num_cpus::get();
    }

    {
        let (job_tx, job_rx) = crossbeam_channel::unbounded::<Job>();
        let (result_tx, result_rx) = crossbeam_channel::unbounded::<ProjectDir>();

        (0..num_threads)
            .map(|_| (job_rx.clone(), result_tx.clone()))
            .for_each(|(job_rx, result_tx)| {
                std::thread::spawn(move || {
                    job_rx
                        .into_iter()
                        .for_each(|job| find_cargo_projects_task(job, result_tx.clone(), verbose))
                });
            });

        job_tx
            .clone()
            .send(Job(path.to_path_buf(), job_tx))
            .unwrap();

        result_rx
    }
    .into_iter()
    .collect()
}

/// Scan the given directory and report to the results Sender if the directory contains a
/// Cargo.toml . Detected subdirectories should be queued as a new job in with the job_sender.
///
/// This function is supposed to be called by the threadpool in find_cargo_projects
fn find_cargo_projects_task(job: Job, results: Sender<ProjectDir>, verbose: bool) {
    let path = job.0;
    let job_sender = job.1;
    let mut has_target = false;

    let read_dir = match path.read_dir() {
        Ok(it) => it,
        Err(e) => {
            verbose.then(|| eprintln!("Error reading directory: '{}'  {}", path.display(), e));
            return;
        }
    };

    let (dirs, files): (Vec<_>, Vec<_>) = read_dir
        .filter_map(|it| it.ok().map(|it| it.path()))
        .partition(|it| it.is_dir());

    let has_cargo_toml = files
        .iter()
        .any(|it| it.file_name().unwrap_or_default().to_string_lossy() == "Cargo.toml");

    // Iterate through the subdirectories of path, ignoring entries that caused errors
    for it in dirs {
        let filename = it.file_name().unwrap_or_default().to_string_lossy();
        match filename.as_ref() {
            // No need to search .git directories for cargo projects. Also skip .cargo directories
            // as there shouldn't be any target dirs in there. Even if there are valid target dirs,
            // they should probably not be deleted. See issue #2 (https://github.com/dnlmlr/cargo-clean-all/issues/2)
            ".git" | ".cargo" => (),
            "target" if has_cargo_toml => has_target = true,
            // For directories queue a new job to search it with the threadpool
            _ => job_sender
                .send(Job(it.to_path_buf(), job_sender.clone()))
                .unwrap(),
        }
    }

    // If path contains a Cargo.toml, it is a project directory
    if has_cargo_toml {
        results.send(ProjectDir(path, has_target)).unwrap();
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
                .unwrap()
                .filter_map(|it| it.ok().map(|it| it.path()))
                .map(Self::recursive_scan_target)
                .fold(default, |a, b| (a.0 + b.0, a.1.max(b.1))),
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
        .replace("\\", "/")
}

impl Display for ProjectTargetAnalysis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let project_name = self
            .project_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let path = pretty_format_path(&std::fs::canonicalize(&self.project_path).unwrap());

        let last_modified: chrono::DateTime<chrono::Local> = self.last_modified.into();
        write!(
            f,
            "{}: {} ({}), {}",
            project_name.bold(),
            bytefmt::format(self.size),
            last_modified.format("%Y-%m-%d %H:%M"),
            path,
        )
    }
}
