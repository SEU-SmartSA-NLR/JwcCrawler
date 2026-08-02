use crate::crawl::NewsItem;
use chrono::NaiveDate;
use std::error::Error;

pub trait DataSource {
    fn fetch(
        &self,
        date_after: Option<NaiveDate>,
        with_contents_only: bool,
    ) -> Result<Vec<NewsItem>, Box<dyn Error>>;
}
