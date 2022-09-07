use clap::Parser;
use crossbeam_channel::Sender;
use std::{
    io::{stdin, stdout, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Debug, Parser)]
#[clap(author, version, about, bin_name = "cargo clean-all", long_about = None)]
struct AppArgs {
    /// The directory that will be cleaned
    #[clap(default_value_t  = String::from("."), value_name = "DIR")]
    root_dir: String,

    /// Don't ask for confirmation
    #[clap(short = 'y', long = "yes")]
    yes: bool,

    /// Don't clean projects with target dir sizes below the specified size
    #[clap(
        short = 's',
        long = "keep-size",
        value_name = "SIZE",
        default_value_t = 0
    )]
    keep_size: u64,

    /// Don't clean projects with target dirs modified in the last [DAYS] days
    #[clap(
        short = 'd',
        long = "keep-days",
        value_name = "DAYS",
        default_value_t = 0
    )]
    keep_last_modified: u32,

    /// Just collect the cleanable project dirs but don't attempt to clean anything
    #[clap(long = "dry-run")]
    dry_run: bool,

    /// The number of threads to use for directory scaning. 0 uses the same amout of theres as CPU
    /// cores
    #[clap(
        short = 't',
        long = "threads",
        value_name = "THREADS",
        default_value_t = 0
    )]
    number_of_threads: usize,
}

fn main() {
    let mut args = std::env::args();

    // When called using `cargo clean-all`, the argument `clean-all` is inserted. To fix the arg
    // alignment, one argument is dropped.
    if let Some("clean-all") = std::env::args().nth(1).as_deref() {
        args.next();
    }

    let args = AppArgs::parse_from(args);

    let scan_path = Path::new(&args.root_dir);

    // Find project dirs
    let project_dirs = find_cargo_projects(scan_path, args.number_of_threads);

    // Analyse project dirs and find out which of them are supposed to be cleaned
    let (mut projects, mut ignored): (Vec<_>, Vec<_>) = project_dirs
        .into_iter()
        .filter_map(|it| it.1.then(|| ProjectTargetAnalysis::analyze(&it.0)))
        .partition(|tgt| {
            let secs_elapsed = tgt
                .last_modified
                .elapsed()
                .unwrap_or_default()
                .as_secs_f32();
            let days_elapsed = secs_elapsed / (60.0 * 60.0 * 24.0);
            days_elapsed >= args.keep_last_modified as f32 && tgt.size > args.keep_size
        });

    projects.sort_by_key(|it| it.size);
    ignored.sort_by_key(|it| it.size);

    let total_size: u64 = projects.iter().map(|it| it.size).sum();

    println!("Ignoring the following project directories:");
    ignored
        .iter()
        .for_each(ProjectTargetAnalysis::print_listformat);

    println!("\nSelected the following project directories for cleaning:");
    projects
        .iter()
        .for_each(ProjectTargetAnalysis::print_listformat);

    println!(
        "\nSelected {}/{} projects, total freeable size: {}",
        projects.len(),
        projects.len() + ignored.len(),
        bytefmt::format(total_size)
    );

    if args.dry_run {
        println!("Dry run. Not doing any cleanup");
        return;
    }

    // Confirm cleanup if --yes is not present in the args
    if !args.yes {
        let mut inp = String::new();
        print!("Clean the project directories shown above? (yes/no): ");
        stdout().flush().unwrap();
        stdin().read_line(&mut inp).unwrap();

        if inp.trim().to_lowercase() != "yes" {
            println!("Cleanup cancelled");
            return;
        }
    }

    println!("Starting cleanup...");

    projects
        .iter()
        .for_each(|p| remove_dir_all::remove_dir_all(&p.project_path.join("target")).unwrap());

    println!("Done!");
}

/// Job for the threaded project finder. First the path to be searched, second the sender to create
/// new jobs for recursively searching the dirs
struct Job(PathBuf, Sender<Job>);

/// Directory of the project and bool that is true if the target directory exists
struct ProjectDir(PathBuf, bool);

/// Recursively scan the given path for cargo projects using the specified number of threads.
///
/// When the number of threads is 0, use as many threads as virtual CPU cores.
fn find_cargo_projects(path: &Path, mut num_threads: usize) -> Vec<ProjectDir> {
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
                        .for_each(|job| find_cargo_projects_task(job, result_tx.clone()))
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
fn find_cargo_projects_task(job: Job, results: Sender<ProjectDir>) {
    let path = job.0;
    let job_sender = job.1;
    let mut has_target = false;

    let read_dir = match path.read_dir() {
        Ok(it) => it,
        Err(e) => {
            eprintln!("Error reading directory: '{}'  {}", path.display(), e);
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
            // No need to search .git directories for cargo projects
            ".git" => (),
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

struct ProjectTargetAnalysis {
    /// The path of the project without the `target` directory suffix
    project_path: PathBuf,
    /// The size in bytes that the target directory takes up
    size: u64,
    /// The timestamp of the last recently modified file in the target directory
    last_modified: SystemTime,
}

impl ProjectTargetAnalysis {
    /// Analyze a given project directories target directory
    pub fn analyze(path: &Path) -> Self {
        let (size, last_modified) = Self::recursive_scan_target(&path.join("target"));
        Self {
            project_path: path.to_owned(),
            size,
            last_modified,
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

    fn print_listformat(&self) {
        let path = std::fs::canonicalize(&self.project_path).unwrap();
        let project_name = path.file_name().unwrap_or_default().to_string_lossy();

        let last_modified: chrono::DateTime<chrono::Local> = self.last_modified.into();
        println!(
            "  {} : {}\n      {}, {}",
            project_name,
            path.display(),
            last_modified.format("%Y-%m-%d %H:%M"),
            bytefmt::format(self.size)
        )
    }
}
