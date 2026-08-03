use crate::hardening::{CrawlBudget, TargetPolicy, atomic_write_json};
use crate::markdown::get_pretty_text;
use crate::models::DataSource;
use chrono::{DateTime, NaiveDate, Utc};
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, LOCATION};
use reqwest::redirect::Policy;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use url::Url;

pub mod cs;
pub mod jwc;
pub mod xsxy;

#[derive(Deserialize, Serialize, Debug)]
pub struct NewsItem {
    pub id: String,
    pub label: String,
    pub title: String,
    pub date: NaiveDate,
    pub detail_url: String,
    pub is_page: bool,
    pub content: Option<Content>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Content {
    pub text: String,
    pub attachment_urls: Vec<String>,
}

#[derive(Eq, Hash, PartialEq, Clone)]
pub struct Category {
    pub label: String,
    pub path: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CategoryFetchReport {
    pub category: String,
    pub status: &'static str,
    pub item_count: usize,
}

pub struct FetchStatus {
    pub news_items: Vec<NewsItem>,
    pub has_next_page: bool,
    pub warning_codes: Vec<String>,
}

pub struct Crawler {
    site_config: SiteConfig,
    client: Client,
    attachment_extensions: Vec<String>,
    crawler_config: CrawlerConfig,
    last_request_at: Mutex<Option<Instant>>,
    last_request_count: Mutex<usize>,
    last_warning_codes: Mutex<Vec<String>>,
    last_category_reports: Mutex<Vec<CategoryFetchReport>>,
}

#[derive(Clone)]
pub struct SiteConfig {
    pub name: String,
    pub base_url: String,
    pub categories: Vec<Category>,
    pub selectors: SelectionConfig,
}

#[derive(Clone)]
pub struct SelectionConfig {
    pub list_row: String,
    pub list_title_link: String,
    pub list_date: String,
    pub content_body: String,
    pub current_page: String,
    pub all_pages: String,
}

#[derive(Clone)]
pub struct CrawlerConfig {
    pub keep_complex_tables: bool,
    pub max_pages_per_category: usize,
    pub max_detail_fetches: usize,
    pub max_total_http_requests: usize,
    pub min_interval: Duration,
    pub max_response_bytes: usize,
    pub deadline_at: Option<DateTime<Utc>>,
    pub cancel_path: Option<PathBuf>,
    pub rate_state_path: Option<PathBuf>,
}

impl CrawlerConfig {
    pub fn bounded(
        keep_complex_tables: bool,
        max_pages_per_category: usize,
        max_detail_fetches: usize,
        max_total_http_requests: usize,
        min_interval_ms: u64,
        max_response_bytes: usize,
    ) -> Result<Self, Box<dyn Error>> {
        CrawlBudget::new(
            max_pages_per_category,
            max_detail_fetches,
            max_total_http_requests,
        )?;
        if max_response_bytes == 0 || max_response_bytes > 64 * 1024 * 1024 {
            return Err("INVALID_RESPONSE_BUDGET".into());
        }
        Ok(Self {
            keep_complex_tables,
            max_pages_per_category,
            max_detail_fetches,
            max_total_http_requests,
            min_interval: Duration::from_millis(min_interval_ms),
            max_response_bytes,
            deadline_at: None,
            cancel_path: None,
            rate_state_path: None,
        })
    }

    pub fn with_runtime_limits(
        mut self,
        deadline_at: DateTime<Utc>,
        cancel_path: PathBuf,
        rate_state_path: PathBuf,
    ) -> Self {
        self.deadline_at = Some(deadline_at);
        self.cancel_path = Some(cancel_path);
        self.rate_state_path = Some(rate_state_path);
        self
    }
}

impl DataSource for Crawler {
    fn fetch(
        &self,
        date_after: Option<NaiveDate>,
        with_contents_only: bool,
    ) -> Result<Vec<NewsItem>, Box<dyn Error>> {
        let mut all_news = Vec::new();
        let mut warning_codes = Vec::new();
        let mut category_reports = Vec::new();
        let mut budget = CrawlBudget::new(
            self.crawler_config.max_pages_per_category,
            self.crawler_config.max_detail_fetches,
            self.crawler_config.max_total_http_requests,
        )?;
        'categories: for category in &self.site_config.categories {
            let item_start = all_news.len();
            let warning_start = warning_codes.len();
            let mut category_failed = false;
            let mut page = 1;
            loop {
                if page > self.crawler_config.max_pages_per_category {
                    break;
                }
                if let Err(error) = budget.record_list_page(&category.label) {
                    warning_codes.push(error.to_string());
                    let item_count = all_news.len() - item_start;
                    category_reports.push(CategoryFetchReport {
                        category: category.label.clone(),
                        status: if item_count == 0 { "failed" } else { "partial" },
                        item_count,
                    });
                    break 'categories;
                }
                let status = match self.fetch_page(
                    category,
                    page,
                    date_after,
                    with_contents_only,
                    &mut budget,
                ) {
                    Ok(status) => status,
                    Err(error) => {
                        let code = error.to_string();
                        if code == "CRAWL_DEADLINE_EXCEEDED" && !all_news.is_empty() {
                            warning_codes.push("CRAWL_DEADLINE_EXCEEDED".to_string());
                            break 'categories;
                        }
                        if matches!(code.as_str(), "CRAWL_DEADLINE_EXCEEDED" | "CRAWL_CANCELLED") {
                            return Err(error);
                        }
                        warning_codes.push(Self::safe_list_warning_code(&code).to_string());
                        category_failed = true;
                        break;
                    }
                };
                warning_codes.extend(status.warning_codes);
                if status.news_items.is_empty() {
                    break;
                }
                all_news.extend(status.news_items);
                if !status.has_next_page {
                    break;
                }
                page += 1;
            }
            let item_count = all_news.len() - item_start;
            let status = if category_failed && item_count == 0 {
                "failed"
            } else if category_failed || warning_codes.len() > warning_start {
                "partial"
            } else {
                "success"
            };
            category_reports.push(CategoryFetchReport {
                category: category.label.clone(),
                status,
                item_count,
            });
        }
        *self
            .last_request_count
            .lock()
            .map_err(|_| "CRAWL_REPORT_UNAVAILABLE")? = budget.total_http_requests();
        warning_codes.sort();
        warning_codes.dedup();
        *self
            .last_warning_codes
            .lock()
            .map_err(|_| "CRAWL_REPORT_UNAVAILABLE")? = warning_codes;
        *self
            .last_category_reports
            .lock()
            .map_err(|_| "CRAWL_REPORT_UNAVAILABLE")? = category_reports;
        Ok(all_news)
    }
}

impl Crawler {
    pub fn new(config: SiteConfig, crawler_config: CrawlerConfig) -> Result<Self, Box<dyn Error>> {
        let user_agent = format!("NLR-JwcCrawler/0.1 ({})", config.name);
        Ok(Self {
            site_config: config,
            client: Client::builder()
                .user_agent(user_agent)
                .timeout(Duration::from_secs(10))
                .redirect(Policy::none())
                .build()?,
            attachment_extensions: [".pdf", ".docx", ".doc", ".xlsx", ".xls", ".zip", ".rar"]
                .iter()
                .map(|value| value.to_string())
                .collect(),
            crawler_config,
            last_request_at: Mutex::new(None),
            last_request_count: Mutex::new(0),
            last_warning_codes: Mutex::new(Vec::new()),
            last_category_reports: Mutex::new(Vec::new()),
        })
    }

    pub fn http_request_count(&self) -> Result<usize, Box<dyn Error>> {
        self.last_request_count
            .lock()
            .map(|value| *value)
            .map_err(|_| "CRAWL_REPORT_UNAVAILABLE".into())
    }

    pub fn category_reports(&self) -> Result<Vec<CategoryFetchReport>, Box<dyn Error>> {
        self.last_category_reports
            .lock()
            .map(|value| value.clone())
            .map_err(|_| "CRAWL_REPORT_UNAVAILABLE".into())
    }

    pub fn warning_codes(&self) -> Result<Vec<String>, Box<dyn Error>> {
        self.last_warning_codes
            .lock()
            .map(|value| value.clone())
            .map_err(|_| "CRAWL_REPORT_UNAVAILABLE".into())
    }

    pub fn fetch_url(&self, url: &str, content_selector: &str) -> Result<Content, Box<dyn Error>> {
        let mut budget = CrawlBudget::new(1, 1, 1)?;
        budget.record_detail()?;
        self.fetch_content(url, content_selector, &mut budget)
    }

    fn fetch_page(
        &self,
        category: &Category,
        page: usize,
        date_after: Option<NaiveDate>,
        with_contents_only: bool,
        budget: &mut CrawlBudget,
    ) -> Result<FetchStatus, Box<dyn Error>> {
        let path = if page == 1 {
            category.path.clone()
        } else {
            category.path.replace("list", &format!("list{page}"))
        };
        let url = format!("{}{}", self.site_config.base_url, path);
        let response_text = self.get_text(&url, budget)?;
        let document = Html::parse_document(&response_text);
        let row_selector = Self::selector(&self.site_config.selectors.list_row)?;
        let link_selector = Self::selector(&self.site_config.selectors.list_title_link)?;
        let date_selector = Self::selector(&self.site_config.selectors.list_date)?;
        let mut items = Vec::new();
        let mut warning_codes = Vec::new();
        let base_url = Url::parse(&self.site_config.base_url)?;
        let mut row_count = 0;
        let mut parsed_row_count = 0;
        let mut detail_budget_exhausted = false;
        for row in document.select(&row_selector) {
            row_count += 1;
            let Some(link) = row.select(&link_selector).next() else {
                continue;
            };
            let Some(title) = link.value().attr("title") else {
                continue;
            };
            let Some(href) = link.value().attr("href") else {
                continue;
            };
            let date_text = row
                .select(&date_selector)
                .next()
                .map(|value| value.text().collect::<String>())
                .unwrap_or_default();
            let Ok(news_date) = NaiveDate::parse_from_str(date_text.trim(), "%Y-%m-%d") else {
                continue;
            };
            parsed_row_count += 1;
            if date_after.is_some_and(|minimum| news_date < minimum) {
                continue;
            }
            let detail_url = match base_url.join(href) {
                Ok(value) => value,
                Err(_) => {
                    warning_codes.push("PARSE_SCHEMA_CHANGED".to_string());
                    continue;
                }
            };
            // 站外链接（公众号文章、其他站点）不是本站 schema 变化，直接跳过
            if detail_url.host_str() != base_url.host_str() {
                continue;
            }
            let detail_url = detail_url.to_string();
            let is_page = !self
                .attachment_extensions
                .iter()
                .any(|extension| detail_url.to_lowercase().ends_with(extension));
            let content = if is_page && !detail_budget_exhausted {
                match budget.record_detail() {
                    Ok(()) => match self.fetch_content(
                        &detail_url,
                        &self.site_config.selectors.content_body,
                        budget,
                    ) {
                        Ok(content) => Some(content),
                        Err(error) => {
                            let code = error.to_string();
                            if matches!(
                                code.as_str(),
                                "CRAWL_DEADLINE_EXCEEDED" | "CRAWL_CANCELLED"
                            ) {
                                return Err(error);
                            }
                            warning_codes.push(Self::safe_warning_code(&code).to_string());
                            None
                        }
                    },
                    Err(error) => {
                        // 详情预算耗尽：只记一次，不再尝试本页剩余详情
                        warning_codes.push(error.to_string());
                        detail_budget_exhausted = true;
                        None
                    }
                }
            } else {
                None
            };
            if with_contents_only && content.is_none() {
                continue;
            }
            items.push(NewsItem {
                id: Self::generate_key(&detail_url),
                label: category.label.clone(),
                title: title.to_string(),
                date: news_date,
                detail_url,
                is_page,
                content,
            });
        }
        if row_count == 0 || parsed_row_count == 0 {
            return Err("PARSE_SCHEMA_CHANGED".into());
        }
        let current_selector = Self::selector(&self.site_config.selectors.current_page)?;
        let all_selector = Self::selector(&self.site_config.selectors.all_pages)?;
        let current = Self::extract_page_number(&document, &current_selector)
            .ok_or("PARSE_SCHEMA_CHANGED")?;
        let total =
            Self::extract_page_number(&document, &all_selector).ok_or("PARSE_SCHEMA_CHANGED")?;
        warning_codes.sort();
        warning_codes.dedup();
        Ok(FetchStatus {
            news_items: items,
            has_next_page: current < total,
            warning_codes,
        })
    }

    fn fetch_content(
        &self,
        url: &str,
        content_selector: &str,
        budget: &mut CrawlBudget,
    ) -> Result<Content, Box<dyn Error>> {
        let text = self.get_text(url, budget)?;
        self.parse_content_html(url, content_selector, &text)
    }

    fn parse_content_html(
        &self,
        url: &str,
        content_selector: &str,
        text: &str,
    ) -> Result<Content, Box<dyn Error>> {
        let base_url = Url::parse(url)?;
        let document = Html::parse_document(text);
        let selector = Self::selector(content_selector)?;
        let content_element = document
            .select(&selector)
            .next()
            .ok_or("PARSE_SCHEMA_CHANGED")?;
        let plain_text = get_pretty_text(
            content_element,
            &base_url,
            self.crawler_config.keep_complex_tables,
        )?;
        let all_elements = Self::selector("*")?;
        let mut attachment_urls = Vec::new();
        for element in content_element.select(&all_elements) {
            for raw_url in [element.value().attr("href"), element.value().attr("pdfsrc")]
                .into_iter()
                .flatten()
            {
                if let Ok(full_url) = base_url.join(raw_url) {
                    let value = full_url.to_string();
                    if self
                        .attachment_extensions
                        .iter()
                        .any(|extension| value.to_lowercase().ends_with(extension))
                        && self.authorize_attachment_reference(&full_url).is_ok()
                    {
                        attachment_urls.push(value);
                    }
                }
            }
        }
        attachment_urls.sort();
        attachment_urls.dedup();
        if plain_text.trim().is_empty() && attachment_urls.is_empty() {
            return Err("PARSE_SCHEMA_CHANGED".into());
        }
        Ok(Content {
            text: plain_text,
            attachment_urls,
        })
    }

    fn get_text(
        &self,
        initial_url: &str,
        budget: &mut CrawlBudget,
    ) -> Result<String, Box<dyn Error>> {
        let mut url = Url::parse(initial_url)?;
        let mut redirect_hops = 0;
        let mut retry_count = 0;
        let mut additional_request = false;
        loop {
            self.ensure_active()?;
            if additional_request {
                budget.record_additional_http_request()?;
            }
            self.authorize(&url)?;
            self.wait_for_interval()?;
            let mut response = self
                .client
                .get(url.clone())
                .timeout(self.request_timeout()?)
                .send()?;
            if response.status().is_redirection() {
                if redirect_hops >= 5 {
                    return Err("REDIRECT_BUDGET_EXCEEDED".into());
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .ok_or("REDIRECT_LOCATION_MISSING")?
                    .to_str()?;
                url = url.join(location)?;
                redirect_hops += 1;
                additional_request = true;
                continue;
            }
            if (response.status().as_u16() == 429 || response.status().is_server_error())
                && retry_count < 1
            {
                retry_count += 1;
                additional_request = true;
                continue;
            }
            if !response.status().is_success() {
                return Err("HTTP_STATUS_REJECTED".into());
            }
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("");
            if content_type.split(';').next().map(str::trim) != Some("text/html") {
                return Err("UNSUPPORTED_CONTENT_TYPE".into());
            }
            if response
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<usize>().ok())
                .is_some_and(|length| length > self.crawler_config.max_response_bytes)
            {
                return Err("RESPONSE_TOO_LARGE".into());
            }
            let mut bytes = Vec::new();
            response
                .by_ref()
                .take(self.crawler_config.max_response_bytes.saturating_add(1) as u64)
                .read_to_end(&mut bytes)?;
            self.ensure_active()?;
            if bytes.len() > self.crawler_config.max_response_bytes {
                return Err("RESPONSE_TOO_LARGE".into());
            }
            return Ok(String::from_utf8(bytes)?);
        }
    }

    fn authorize(&self, url: &Url) -> Result<(), Box<dyn Error>> {
        if self.site_config.base_url == "https://jwc.seu.edu.cn" {
            TargetPolicy::jwc().authorize(url)?;
            return Ok(());
        }
        let base = Url::parse(&self.site_config.base_url)?;
        if url.scheme() != "https"
            || url.host_str() != base.host_str()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("TARGET_NOT_ALLOWLISTED".into());
        }
        Ok(())
    }

    fn authorize_attachment_reference(&self, url: &Url) -> Result<(), Box<dyn Error>> {
        if self.site_config.base_url == "https://jwc.seu.edu.cn" {
            TargetPolicy::jwc().authorize_attachment_reference(url)?;
            return Ok(());
        }
        let base = Url::parse(&self.site_config.base_url)?;
        if url.scheme() != "https"
            || url.host_str() != base.host_str()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
        {
            return Err("ATTACHMENT_REFERENCE_REJECTED".into());
        }
        Ok(())
    }

    fn wait_for_interval(&self) -> Result<(), Box<dyn Error>> {
        let mut last_request = self
            .last_request_at
            .lock()
            .map_err(|_| "LIMITER_STATE_UNAVAILABLE")?;
        let mut wait = Duration::ZERO;
        if let Some(previous) = *last_request {
            wait = self
                .crawler_config
                .min_interval
                .saturating_sub(previous.elapsed());
        }
        let now_millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        if let Some(path) = &self.crawler_config.rate_state_path
            && let Ok(value) = fs::read_to_string(path)
            && let Ok(previous_millis) = value.trim().parse::<u128>()
        {
            let elapsed_millis = now_millis.saturating_sub(previous_millis);
            let persisted_wait =
                self.crawler_config
                    .min_interval
                    .saturating_sub(Duration::from_millis(
                        elapsed_millis.min(u64::MAX as u128) as u64
                    ));
            wait = wait.max(persisted_wait);
        }
        if !wait.is_zero() {
            if self.request_timeout()? <= wait {
                return Err("CRAWL_DEADLINE_EXCEEDED".into());
            }
            thread::sleep(wait);
            self.ensure_active()?;
        }
        if let Some(path) = &self.crawler_config.rate_state_path {
            let current_millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
            atomic_write_json(path, &current_millis)?;
        }
        *last_request = Some(Instant::now());
        Ok(())
    }

    fn ensure_active(&self) -> Result<(), Box<dyn Error>> {
        if self
            .crawler_config
            .cancel_path
            .as_ref()
            .is_some_and(|path| path.exists())
        {
            return Err("CRAWL_CANCELLED".into());
        }
        if self
            .crawler_config
            .deadline_at
            .is_some_and(|deadline| Utc::now() >= deadline)
        {
            return Err("CRAWL_DEADLINE_EXCEEDED".into());
        }
        Ok(())
    }

    fn request_timeout(&self) -> Result<Duration, Box<dyn Error>> {
        let default = Duration::from_secs(10);
        let Some(deadline) = self.crawler_config.deadline_at else {
            return Ok(default);
        };
        let remaining = (deadline - Utc::now())
            .to_std()
            .map_err(|_| "CRAWL_DEADLINE_EXCEEDED")?;
        if remaining.is_zero() {
            return Err("CRAWL_DEADLINE_EXCEEDED".into());
        }
        Ok(default.min(remaining))
    }

    fn selector(value: &str) -> Result<Selector, Box<dyn Error>> {
        Selector::parse(value).map_err(|_| "PARSE_SCHEMA_CHANGED".into())
    }

    fn extract_page_number(document: &Html, selector: &Selector) -> Option<i32> {
        document
            .select(selector)
            .next()
            .and_then(|value| value.text().collect::<String>().trim().parse::<i32>().ok())
    }

    fn safe_list_warning_code(code: &str) -> &'static str {
        match code {
            "TOTAL_REQUEST_BUDGET_EXCEEDED" => "TOTAL_REQUEST_BUDGET_EXCEEDED",
            "TARGET_NOT_ALLOWLISTED" => "TARGET_NOT_ALLOWLISTED",
            "RESPONSE_TOO_LARGE" => "RESPONSE_TOO_LARGE",
            "UNSUPPORTED_CONTENT_TYPE" => "UNSUPPORTED_CONTENT_TYPE",
            "PARSE_SCHEMA_CHANGED" => "PARSE_SCHEMA_CHANGED",
            "HTTP_STATUS_REJECTED" => "HTTP_STATUS_REJECTED",
            "REDIRECT_BUDGET_EXCEEDED" => "REDIRECT_BUDGET_EXCEEDED",
            _ => "LIST_FETCH_FAILED",
        }
    }

    fn safe_warning_code(code: &str) -> &'static str {
        match code {
            "DETAIL_BUDGET_EXCEEDED" => "DETAIL_BUDGET_EXCEEDED",
            "TOTAL_REQUEST_BUDGET_EXCEEDED" => "TOTAL_REQUEST_BUDGET_EXCEEDED",
            "TARGET_NOT_ALLOWLISTED" => "TARGET_NOT_ALLOWLISTED",
            "RESPONSE_TOO_LARGE" => "RESPONSE_TOO_LARGE",
            "UNSUPPORTED_CONTENT_TYPE" => "UNSUPPORTED_CONTENT_TYPE",
            "PARSE_SCHEMA_CHANGED" => "PARSE_SCHEMA_CHANGED",
            "MARKDOWN_CONVERSION_FAILED" => "MARKDOWN_CONVERSION_FAILED",
            "HTTP_STATUS_REJECTED" => "HTTP_STATUS_REJECTED",
            "REDIRECT_BUDGET_EXCEEDED" => "REDIRECT_BUDGET_EXCEEDED",
            _ => "DETAIL_FETCH_FAILED",
        }
    }

    fn generate_key(url: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(url.as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|value| format!("{value:02x}"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crawl::jwc::get_jwc;
    use chrono::Utc;

    #[test]
    fn fetch_rejects_elapsed_deadline_before_any_http_request() {
        let crawler_config = CrawlerConfig::bounded(true, 2, 20, 40, 1_000, 4_194_304)
            .unwrap()
            .with_runtime_limits(
                Utc::now() - chrono::Duration::seconds(1),
                PathBuf::from("/nonexistent-cancel"),
                PathBuf::from("/nonexistent-rate"),
            );
        let crawler = get_jwc(crawler_config).unwrap();

        let error = crawler.fetch(None, true).unwrap_err();

        assert_eq!(error.to_string(), "CRAWL_DEADLINE_EXCEEDED");
    }

    #[test]
    fn external_list_links_are_detected_by_host_mismatch() {
        // 列表页会混入公众号与兄弟站点链接，这些链接必须按站外跳过，
        // 而不是当作本站详情页触发 TARGET_NOT_ALLOWLISTED。
        let base_url = Url::parse("https://jwc.seu.edu.cn").unwrap();
        for (href, expected_internal) in [
            ("/2026/0729/c21678a578468/page.htm", true),
            ("/_upload/file/demo.pdf", true),
            ("https://mp.weixin.qq.com/s/8v8FjMNAEsGxZRkP5EUWng", false),
            ("https://power.seu.edu.cn/2026/0430/c9503a566327/page.htm", false),
        ] {
            let joined = base_url.join(href).unwrap();
            assert_eq!(
                joined.host_str() == base_url.host_str(),
                expected_internal,
                "href={href}"
            );
        }
    }
}
