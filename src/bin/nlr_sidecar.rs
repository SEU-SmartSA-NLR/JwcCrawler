use clap::Parser;
use jwc_crawler::worker::{prepare_private_spool, process_ready_job, recover_interrupted_jobs};
use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

#[derive(Parser)]
struct SidecarArgs {
    #[arg(long)]
    spool_dir: PathBuf,
    #[arg(long)]
    once: bool,
}

struct WorkerLease {
    _file: File,
}

impl WorkerLease {
    fn acquire(spool_dir: &Path) -> Result<Self, Box<dyn Error>> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(spool_dir.join(".worker.lock"))?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err("SIDECAR_WORKER_ALREADY_RUNNING".into());
        }
        Ok(Self { _file: file })
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = SidecarArgs::parse();
    prepare_private_spool(&args.spool_dir)?;
    let _lease = WorkerLease::acquire(&args.spool_dir)?;
    recover_interrupted_jobs(&args.spool_dir)?;
    let requests = args.spool_dir.join("requests");
    loop {
        let mut ready_files = fs::read_dir(&requests)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("ready"))
            .collect::<Vec<_>>();
        ready_files.sort();
        for ready_path in &ready_files {
            if process_ready_job(&args.spool_dir, ready_path).is_err() {
                eprintln!("spool job was rejected safely");
            }
            thread::sleep(Duration::from_secs(1));
        }
        if args.once {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
}
