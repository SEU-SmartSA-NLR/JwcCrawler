use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::error::Error;
use std::ffi::CString;
use std::fmt::{Display, Formatter};
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use url::Url;

#[derive(Debug)]
pub struct HardeningError(&'static str);

impl Display for HardeningError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for HardeningError {}

pub struct CrawlBudget {
    max_pages_per_category: usize,
    max_detail_fetches: usize,
    max_total_http_requests: usize,
    pages_by_category: HashMap<String, usize>,
    details_by_category: HashMap<String, usize>,
    detail_quota: usize,
    detail_fetches: usize,
    total_http_requests: usize,
}

impl CrawlBudget {
    pub fn new(
        max_pages_per_category: usize,
        max_detail_fetches: usize,
        max_total_http_requests: usize,
        category_count: usize,
    ) -> Result<Self, HardeningError> {
        if max_pages_per_category == 0
            || max_detail_fetches == 0
            || max_total_http_requests < max_detail_fetches
            || category_count == 0
        {
            return Err(HardeningError("INVALID_CRAWL_BUDGET"));
        }
        Ok(Self {
            max_pages_per_category,
            max_detail_fetches,
            max_total_http_requests,
            pages_by_category: HashMap::new(),
            details_by_category: HashMap::new(),
            // 详情预算按分类均分（向上取整），避免第一个分类吃光全部预算
            detail_quota: max_detail_fetches.div_ceil(category_count),
            detail_fetches: 0,
            total_http_requests: 0,
        })
    }

    pub fn record_list_page(&mut self, category: &str) -> Result<(), HardeningError> {
        let current = self.pages_by_category.get(category).copied().unwrap_or(0);
        if current >= self.max_pages_per_category {
            return Err(HardeningError("LIST_PAGE_BUDGET_EXCEEDED"));
        }
        self.record_http_request()?;
        self.pages_by_category
            .insert(category.to_string(), current + 1);
        Ok(())
    }

    pub fn record_detail(&mut self, category: &str) -> Result<(), HardeningError> {
        if self.detail_fetches >= self.max_detail_fetches {
            return Err(HardeningError("DETAIL_BUDGET_EXCEEDED"));
        }
        let used = self.details_by_category.get(category).copied().unwrap_or(0);
        let quota = if category == "__single__" {
            self.max_detail_fetches
        } else {
            self.detail_quota
        };
        if used >= quota {
            return Err(HardeningError("DETAIL_BUDGET_EXCEEDED"));
        }
        self.record_http_request()?;
        self.detail_fetches += 1;
        self.details_by_category.insert(category.to_string(), used + 1);
        Ok(())
    }

    pub fn total_http_requests(&self) -> usize {
        self.total_http_requests
    }

    pub fn record_additional_http_request(&mut self) -> Result<(), HardeningError> {
        self.record_http_request()
    }

    fn record_http_request(&mut self) -> Result<(), HardeningError> {
        if self.total_http_requests >= self.max_total_http_requests {
            return Err(HardeningError("TOTAL_REQUEST_BUDGET_EXCEEDED"));
        }
        self.total_http_requests += 1;
        Ok(())
    }
}

pub struct TargetPolicy {
    host: &'static str,
    list_path: Regex,
    detail_path: Regex,
}

impl TargetPolicy {
    pub fn jwc() -> Self {
        Self {
            host: "jwc.seu.edu.cn",
            list_path: Regex::new(
                r"^/(?:zxdt|jwxx|xjgl|jxyj|sjjx|gjjl|cbxx)/list(?:[0-9]+)?\.(?:htm|psp)$",
            )
            .expect("固定列表规则必须有效"),
            detail_path: Regex::new(r"^/[0-9]{4}/[0-9]{4}/c[0-9a-f]+/page\.htm$")
                .expect("固定详情规则必须有效"),
        }
    }

    pub fn authorize(&self, url: &Url) -> Result<(), HardeningError> {
        if url.scheme() != "https"
            || url.host_str() != Some(self.host)
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || !(self.list_path.is_match(url.path()) || self.detail_path.is_match(url.path()))
        {
            return Err(HardeningError("TARGET_NOT_ALLOWLISTED"));
        }
        Ok(())
    }

    pub fn authorize_attachment_reference(&self, url: &Url) -> Result<(), HardeningError> {
        let approved_extension = [".pdf", ".doc", ".docx", ".xls", ".xlsx", ".zip", ".rar"]
            .iter()
            .any(|extension| url.path().to_ascii_lowercase().ends_with(extension));
        if url.scheme() != "https"
            || url.host_str() != Some(self.host)
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || !approved_extension
        {
            return Err(HardeningError("ATTACHMENT_REFERENCE_REJECTED"));
        }
        Ok(())
    }
}

struct LimitedWriter<W: Write> {
    inner: W,
    remaining: usize,
}

impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.remaining {
            return Err(io::Error::other("RESULT_TOO_LARGE"));
        }
        let written = self.inner.write(buffer)?;
        self.remaining -= written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Box<dyn Error>> {
    atomic_write_json_limited(path, value, 64 * 1024 * 1024)
}

pub fn atomic_write_json_limited<T: Serialize>(
    path: &Path,
    value: &T,
    max_bytes: usize,
) -> Result<(), Box<dyn Error>> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent)?;
    let final_name = CString::new(path.file_name().ok_or("OUTPUT_PATH_INVALID")?.as_bytes())?;
    let mut temporary_bytes = final_name.as_bytes().to_vec();
    temporary_bytes.extend_from_slice(b".tmp");
    let temporary_name = CString::new(temporary_bytes)?;
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            temporary_name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let file = unsafe { File::from_raw_fd(descriptor) };
    let result = (|| -> Result<(), Box<dyn Error>> {
        let limited = LimitedWriter {
            inner: file,
            remaining: max_bytes,
        };
        let mut writer = BufWriter::new(limited);
        serde_json::to_writer_pretty(&mut writer, value)?;
        writer.flush()?;
        writer.get_ref().inner.sync_all()?;
        let renamed = unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                temporary_name.as_ptr(),
                directory.as_raw_fd(),
                final_name.as_ptr(),
            )
        };
        if renamed != 0 {
            return Err(io::Error::last_os_error().into());
        }
        directory.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        unsafe {
            libc::unlinkat(directory.as_raw_fd(), temporary_name.as_ptr(), 0);
        }
    }
    result
}
