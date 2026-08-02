use crate::crawl::cs::get_cs;
use crate::crawl::jwc::get_jwc;
use crate::crawl::xsxy::get_xsxy;
use crate::crawl::{Crawler, CrawlerConfig};
use crate::hardening::atomic_write_json;
use crate::models::DataSource;
use chrono::NaiveDate;
use clap::Parser;
use std::collections::HashMap;
use std::error::Error;

mod crawl;
pub mod hardening;
mod markdown;
pub mod models;
pub mod worker;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    #[arg(short, long, help = "Output file path")]
    out: String,
    #[arg(
        long,
        default_value = "jwc",
        help = "Data Sources, e.g. jwc, xsxy, etc."
    )]
    data_source: String,
    #[arg(
        short,
        long,
        help = "Fetch news after the given date. Fetch all if not passed. e.g. 2026-03-01"
    )]
    date: Option<String>,
    #[arg(long, help = "Only fetch news with contents")]
    with_contents_only: bool,
    #[arg(
        long,
        help = "Keep complex tables (with rowspan/colspan) as HTML instead of converting to Markdown"
    )]
    keep_complex_tables: bool,
    #[arg(long, help = "Fetch a single URL instead of crawling data source")]
    url: Option<String>,
    #[arg(long, default_value_t = 2)]
    max_pages_per_category: usize,
    #[arg(long, default_value_t = 20)]
    max_detail_fetches: usize,
    #[arg(long, default_value_t = 40)]
    max_total_http_requests: usize,
    #[arg(long, default_value_t = 1000)]
    min_interval_ms: u64,
    #[arg(long, default_value_t = 4_194_304)]
    max_response_bytes: usize,
}

type CrawlerFactory = fn(CrawlerConfig) -> Result<Crawler, Box<dyn Error>>;

pub fn run(args: Args) -> Result<(), Box<dyn Error>> {
    let crawler_config = CrawlerConfig::bounded(
        args.keep_complex_tables,
        args.max_pages_per_category,
        args.max_detail_fetches,
        args.max_total_http_requests,
        args.min_interval_ms,
        args.max_response_bytes,
    )?;

    if let Some(url) = args.url {
        let crawler = get_jwc(crawler_config)?;
        let content = crawler.fetch_url(&url, "div.Article_Content")?;
        atomic_write_json(std::path::Path::new(&args.out), &content)?;
        return Ok(());
    }

    let crawler_map: HashMap<String, CrawlerFactory> = HashMap::from([
        ("jwc".to_string(), get_jwc as CrawlerFactory),
        ("xsxy".to_string(), get_xsxy as CrawlerFactory),
        ("cs".to_string(), get_cs as CrawlerFactory),
    ]);
    let factory = crawler_map.get(&args.data_source).ok_or_else(|| {
        format!(
            "Unsupported data source: {}. Currently support {}.",
            args.data_source,
            crawler_map.keys().cloned().collect::<Vec<_>>().join(", ")
        )
    })?;
    let crawler = factory(crawler_config)?;
    let date = args
        .date
        .map(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d"))
        .transpose()?;

    let items = crawler.fetch(date, args.with_contents_only)?;

    atomic_write_json(std::path::Path::new(&args.out), &items)?;
    Ok(())
}
