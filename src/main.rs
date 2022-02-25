use std::ffi::OsStr;
use std::io::{stdin, stdout, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const HELP_TEXT: &str = r#"
Clean all rust project target directories under a given directory.

Usage:
    cargo clean-all [OPTIONS]

Options:
    --help             Display this help text
    --dir [DIR]        Clean [DIR] instead of working dir
    --yes              Perform the cleaning without asking first
    --dry-run          Just list the cleanable projects, don't ask to actually perform cleaning
    --keep-size [SIZE] Keep target dirs with size below [SIZE]
    --keep-days [DAYS] Keep target dirs modified less than [DAYS] days ago

Examples:
    # Clean all projects in the current directory with a target-dir size of 500MB or more
      ~/projects $ cargo clean-all --keep-size 500MB

    # Clean all projects in the "~/projects" directory that were last compiled 7 or more days ago
      / $ cargo clean-all --dir ~/projects --keep-days 7
"#;

#[derive(Debug)]
struct AppArgs {
    root_dir: String,
    yes: bool,
    keep_size: u64,
    keep_last_modified: u32,
    dry_run: bool,
}

fn parse_args() -> Result<AppArgs, pico_args::Error> {
    let mut pargs = pico_args::Arguments::from_env();

    if pargs.contains("--help") {
        print_help_and_exit();
    }

    Ok(AppArgs {
        root_dir: pargs
            .opt_value_from_str("--dir")?
            .unwrap_or(".".to_string()),
        yes: pargs.contains("--yes"),
        keep_size: pargs
            .opt_value_from_fn("--keep-size", parse_arg_opt_filesize)?
            .unwrap_or(0),
        keep_last_modified: pargs.opt_value_from_str("--keep-days")?.unwrap_or(0),
        dry_run: pargs.contains("--dry-run"),
    })
}

fn parse_arg_opt_filesize(arg: &str) -> Result<u64, &'static str> {
    bytefmt::parse(arg)
}

fn print_help_and_exit() {
    println!(
        "cargo-clean-all v{}{}",
        env!("CARGO_PKG_VERSION"),
        HELP_TEXT
    );
    std::process::exit(0);
}

fn main() {
    let args = parse_args().unwrap();

    let scan_path = Path::new(&args.root_dir);

    let (mut projects, mut ignored): (Vec<_>, Vec<_>) = find_cargo_projects(scan_path)
        .into_iter()
        .map(|it| ProjectTargetAnalysis::analyze(&it))
        .partition(|it| {
            let secs_elapsed = it
                .last_modified
                .elapsed()
                .expect(&format!(
                    "Timestamp calculation failed: {}",
                    it.project_path.display()
                ))
                .as_secs_f32();

            let days_elapsed = secs_elapsed / (60.0 * 60.0 * 24.0);

            if days_elapsed < args.keep_last_modified as f32 || args.keep_size > it.size {
                false
            } else {
                true
            }
        });

    projects.sort_by_key(|it| it.size);

    ignored.sort_by_key(|it| it.size);

    let total_size: u64 = projects.iter().map(|it| it.size).sum();

    println!("Ignoring the following project directories:");

    for p in &ignored {
        p.print_listformat();
    }

    println!("\nSelected the following project directories for cleaning:");

    for p in &projects {
        p.print_listformat();
    }

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

    for p in &projects {
        std::fs::remove_dir_all(&p.project_path.join("target")).unwrap();
    }

    println!("Done!");
}

/// Detect rust project directories in the given parent path that have `target` directories in them.
fn find_cargo_projects(path: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    let mut has_target = false;
    let mut has_cargo_toml = false;

    let read_dir = match path.read_dir() {
        Ok(read_dir) => read_dir,
        Err(e) => {
            eprintln!("Error reading directory: '{}'  {}", path.display(), e);
            return dirs;
        }
    };

    for it in read_dir.filter_map(|it| it.ok().map(|it| it.path())) {
        if it.is_dir() {
            if it.file_name() == Some(OsStr::new("target")) {
                has_target = true;
            } else if it.file_name() != Some(OsStr::new(".git")) {
                dirs.extend(find_cargo_projects(&it));
            }
        } else {
            if it.file_name() == Some(OsStr::new("Cargo.toml")) {
                has_cargo_toml = true;
            }
        }
    }

    if has_target && has_cargo_toml {
        dirs.push(path.to_owned());
    }

    dirs
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
    fn recursive_scan_target(path: &Path) -> (u64, SystemTime) {
        let mut size = 0;
        let mut last_modified = SystemTime::UNIX_EPOCH;

        path.read_dir()
            .unwrap()
            .filter_map(|it| it.ok().map(|it| it.path()))
            .for_each(|entry| {
                if entry.is_dir() {
                    let dir_stats = Self::recursive_scan_target(&entry);

                    size += dir_stats.0;
                    if dir_stats.1 > last_modified {
                        last_modified = dir_stats.1;
                    }
                } else if let Ok(md) = entry.metadata() {
                    size += md.len();
                    if let Ok(modified) = md.modified() {
                        if modified > last_modified {
                            last_modified = modified;
                        }
                    }
                }
            });

        (size, last_modified)
    }

    fn print_listformat(&self) {
        let path = std::fs::canonicalize(&self.project_path).unwrap();
        let project_name = path.file_name().unwrap_or_default().to_string_lossy();

        let last_modified: chrono::DateTime<chrono::Local> = self.last_modified.into();

        println!(
            "  {} : {}\n      {}, {}",
            project_name,
            self.project_path.display(),
            last_modified.format("%Y-%m-%d %H:%M"),
            bytefmt::format(self.size)
        )
    }
}
