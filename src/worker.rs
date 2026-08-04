use crate::crawl::jwc::get_jwc;
use crate::crawl::{CategoryFetchReport, CrawlerConfig};
use crate::hardening::{atomic_write_json, atomic_write_json_limited};
use crate::models::DataSource;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs::{self, DirBuilder, OpenOptions, Permissions};
use std::io::{BufReader, Read};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerJob {
    pub schema_version: u8,
    pub job_id: String,
    pub source_id: String,
    pub created_at: DateTime<Utc>,
    pub deadline_at: DateTime<Utc>,
    pub date_after: Option<NaiveDate>,
    pub date_before: Option<NaiveDate>,
    pub category_label: Option<String>,
    pub start_page: Option<usize>,
    pub max_pages_per_category: usize,
    pub max_detail_fetches: usize,
    pub max_total_http_requests: usize,
    pub with_contents_only: bool,
}

impl WorkerJob {
    pub fn validate(&self, now: DateTime<Utc>) -> Result<(), &'static str> {
        if !(self.schema_version == 1 || self.schema_version == 2) || self.source_id != "seu-jwc" {
            return Err("JOB_SCHEMA_INVALID");
        }
        if self.job_id.is_empty()
            || !self
                .job_id
                .chars()
                .all(|value| value.is_ascii_alphanumeric() || "._-".contains(value))
        {
            return Err("JOB_ID_INVALID");
        }
        if self.start_page.is_some_and(|page| page == 0)
            || self
                .date_after
                .zip(self.date_before)
                .is_some_and(|(start, end)| start > end)
        {
            return Err("JOB_CURSOR_INVALID");
        }
        if self.schema_version == 2 && self.category_label.as_deref().is_none_or(str::is_empty) {
            return Err("JOB_CATEGORY_INVALID");
        }
        if self.created_at >= self.deadline_at
            || now >= self.deadline_at
            || self.deadline_at - self.created_at > chrono::Duration::seconds(120)
        {
            return Err("CRAWL_DEADLINE_EXCEEDED");
        }
        let max_detail_fetches = if self.schema_version == 2 { 50 } else { 20 };
        let max_total_http_requests = if self.schema_version == 2 { 60 } else { 40 };
        if self.max_pages_per_category == 0
            || self.max_pages_per_category > 2
            || self.max_detail_fetches == 0
            || self.max_detail_fetches > max_detail_fetches
            || self.max_total_http_requests < self.max_detail_fetches
            || self.max_total_http_requests > max_total_http_requests
            || !self.with_contents_only
        {
            return Err("JOB_BUDGET_INVALID");
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct WorkerStatus {
    pub schema_version: u8,
    pub job_id: String,
    pub source_id: String,
    pub status: &'static str,
    pub warning_codes: Vec<String>,
}

impl WorkerStatus {
    pub fn failed(job_id: &str, source_id: &str, code: &str) -> Self {
        Self {
            schema_version: 1,
            job_id: job_id.to_string(),
            source_id: source_id.to_string(),
            status: "failed",
            warning_codes: vec![code.to_string()],
        }
    }
}

#[derive(Debug, Serialize)]
pub struct WorkerResultManifest {
    pub schema_version: u8,
    pub job_id: String,
    pub source_id: String,
    pub status: &'static str,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub http_request_count: usize,
    pub items_file: String,
    pub items_sha256: String,
    pub request_sha256: String,
    pub category_reports: Vec<CategoryFetchReport>,
    pub warning_codes: Vec<String>,
}

pub fn prepare_private_spool(root: &Path) -> Result<(), Box<dyn Error>> {
    if !root.exists() {
        DirBuilder::new().recursive(true).mode(0o700).create(root)?;
    }
    for path in std::iter::once(root.to_path_buf()).chain(
        ["requests", "running", "results", "cancel"]
            .iter()
            .map(|value| root.join(value)),
    ) {
        if !path.exists() {
            DirBuilder::new().mode(0o700).create(&path)?;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err("SPOOL_DIRECTORY_UNSAFE".into());
        }
        fs::set_permissions(&path, Permissions::from_mode(0o700))?;
    }
    let stale_rate_temporary = root.join(".last_request_at.json.tmp");
    if let Ok(metadata) = fs::symlink_metadata(&stale_rate_temporary) {
        if metadata.file_type().is_dir() {
            return Err("SPOOL_DIRECTORY_UNSAFE".into());
        }
        fs::remove_file(stale_rate_temporary)?;
    }
    Ok(())
}

pub fn recover_interrupted_jobs(root: &Path) -> Result<usize, Box<dyn Error>> {
    prepare_private_spool(root)?;
    let mut recovered = 0;
    for entry in fs::read_dir(root.join("running"))? {
        let path = entry?.path();
        let Some(job_id) = path
            .file_name()
            .and_then(|value| value.to_str())
            .and_then(|value| value.strip_suffix(".running"))
        else {
            continue;
        };
        if job_id.is_empty()
            || !job_id
                .chars()
                .all(|value| value.is_ascii_alphanumeric() || "._-".contains(value))
        {
            continue;
        }
        let results = root.join("results");
        let done = results.join(format!("{job_id}.done"));
        let failed = results.join(format!("{job_id}.failed"));
        if !done.exists() && !failed.exists() {
            for suffix in ["items.json", "items.json.tmp", "done.tmp", "failed.tmp"] {
                let _ = fs::remove_file(results.join(format!("{job_id}.{suffix}")));
            }
            atomic_write_json(
                &failed,
                &WorkerStatus::failed(job_id, "seu-jwc", "WORKER_RESTARTED"),
            )?;
        }
        fs::remove_file(path)?;
        recovered += 1;
    }
    Ok(recovered)
}

pub fn process_ready_job(root: &Path, ready_path: &Path) -> Result<(), Box<dyn Error>> {
    prepare_private_spool(root)?;
    if ready_path.is_symlink() {
        return Err("JOB_PATH_INVALID".into());
    }
    let file_name = ready_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("JOB_PATH_INVALID")?;
    let job_id = file_name.strip_suffix(".ready").ok_or("JOB_PATH_INVALID")?;
    if job_id.is_empty()
        || !job_id
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || "._-".contains(value))
    {
        return Err("JOB_ID_INVALID".into());
    }
    let running = root.join("running").join(format!("{job_id}.running"));
    let items = root.join("results").join(format!("{job_id}.items.json"));
    let done_temporary = root.join("results").join(format!("{job_id}.done.tmp"));
    let done = root.join("results").join(format!("{job_id}.done"));
    let failed = root.join("results").join(format!("{job_id}.failed"));
    if [&running, &items, &done_temporary, &done, &failed]
        .iter()
        .any(|path| path.exists() || path.is_symlink())
    {
        fs::remove_file(ready_path)?;
        return Err("JOB_REPLAYED".into());
    }
    fs::rename(ready_path, &running)?;
    let claimed = fs::symlink_metadata(&running)?;
    if !claimed.file_type().is_file() || claimed.len() > 256 * 1024 {
        let _ = fs::remove_file(&running);
        atomic_write_json(
            &failed,
            &WorkerStatus::failed(job_id, "seu-jwc", "JOB_SCHEMA_INVALID"),
        )?;
        return Ok(());
    }
    let cancelled = root.join("cancel").join(format!("{job_id}.cancel"));
    let result = if cancelled.exists() {
        Err("CRAWL_CANCELLED".into())
    } else {
        run_job_file(&running, &items, &done).map(|_| ())
    };
    if let Err(error) = result {
        let _ = fs::remove_file(&items);
        let stable_code = match error.to_string().as_str() {
            "CRAWL_CANCELLED" => "CRAWL_CANCELLED",
            "CRAWL_DEADLINE_EXCEEDED" => "CRAWL_DEADLINE_EXCEEDED",
            "JOB_SCHEMA_INVALID" => "JOB_SCHEMA_INVALID",
            "JOB_ID_INVALID" => "JOB_ID_INVALID",
            "JOB_BUDGET_INVALID" => "JOB_BUDGET_INVALID",
            "JOB_CATEGORY_INVALID" => "JOB_CATEGORY_INVALID",
            "JOB_CURSOR_INVALID" => "JOB_CURSOR_INVALID",
            _ => "CRAWL_FAILED",
        };
        atomic_write_json(
            &failed,
            &WorkerStatus::failed(job_id, "seu-jwc", stable_code),
        )?;
    }
    let _ = fs::remove_file(running);
    Ok(())
}

pub fn run_job_file(
    job_path: &Path,
    items_path: &Path,
    manifest_path: &Path,
) -> Result<WorkerResultManifest, Box<dyn Error>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(job_path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.len() > 256 * 1024 {
        return Err("JOB_SCHEMA_INVALID".into());
    }
    let mut payload = Vec::with_capacity(metadata.len() as usize);
    file.take(256 * 1024 + 1).read_to_end(&mut payload)?;
    if payload.len() > 256 * 1024 {
        return Err("JOB_SCHEMA_INVALID".into());
    }
    let request_sha256 = Sha256::digest(&payload)
        .iter()
        .map(|value| format!("{value:02x}"))
        .collect();
    let job: WorkerJob = serde_json::from_slice(&payload)?;
    job.validate(Utc::now())?;
    let expected_job_id = items_path
        .file_name()
        .and_then(|value| value.to_str())
        .and_then(|value| value.strip_suffix(".items.json"))
        .ok_or("JOB_PATH_INVALID")?;
    if expected_job_id != job.job_id {
        return Err("JOB_ID_INVALID".into());
    }
    let cancel_path = manifest_path
        .parent()
        .and_then(|results| results.parent())
        .ok_or("JOB_PATH_INVALID")?
        .join("cancel")
        .join(format!("{}.cancel", job.job_id));
    let rate_state_path = cancel_path
        .parent()
        .and_then(|cancel| cancel.parent())
        .ok_or("JOB_PATH_INVALID")?
        .join(".last_request_at.json");
    let config = CrawlerConfig::bounded(
        false,
        job.max_pages_per_category,
        job.max_detail_fetches,
        job.max_total_http_requests,
        1_000,
        4_194_304,
    )?
    .with_runtime_limits(job.deadline_at, cancel_path.clone(), rate_state_path)
    .with_scan_window(
        job.start_page.unwrap_or(1),
        job.category_label.clone(),
        job.date_before,
    )?;
    let crawler = get_jwc(config)?;
    let started_at = Utc::now();
    let items = crawler.fetch(job.date_after, job.with_contents_only)?;
    ensure_job_active(&job, &cancel_path)?;
    atomic_write_json_limited(items_path, &items, 8 * 1024 * 1024)?;
    ensure_job_active(&job, &cancel_path)?;
    let items_file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(items_path)?;
    let items_metadata = items_file.metadata()?;
    if !items_metadata.file_type().is_file() || items_metadata.len() > 8 * 1024 * 1024 {
        return Err("RESULT_TOO_LARGE".into());
    }
    let mut reader = BufReader::new(items_file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let items_sha256 = hasher
        .finalize()
        .iter()
        .map(|value| format!("{value:02x}"))
        .collect();
    ensure_job_active(&job, &cancel_path)?;
    let items_file = items_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("JOB_PATH_INVALID")?
        .to_string();
    let warning_codes = crawler.warning_codes()?;
    let category_reports = crawler.category_reports()?;
    let status = if warning_codes.is_empty() {
        "complete"
    } else {
        "partial"
    };
    let completed_at = Utc::now();
    ensure_job_active(&job, &cancel_path)?;
    let manifest = WorkerResultManifest {
        schema_version: job.schema_version,
        job_id: job.job_id,
        source_id: job.source_id,
        status,
        started_at,
        completed_at,
        http_request_count: crawler.http_request_count()?,
        items_file,
        items_sha256,
        request_sha256,
        category_reports,
        warning_codes,
    };
    atomic_write_json(manifest_path, &manifest)?;
    Ok(manifest)
}

fn ensure_job_active(job: &WorkerJob, cancel_path: &Path) -> Result<(), Box<dyn Error>> {
    if cancel_path.exists() {
        return Err("CRAWL_CANCELLED".into());
    }
    if Utc::now() >= job.deadline_at {
        return Err("CRAWL_DEADLINE_EXCEEDED".into());
    }
    Ok(())
}
