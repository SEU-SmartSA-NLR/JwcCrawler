use crate::crawl::{Category, Crawler, CrawlerConfig, SelectionConfig, SiteConfig};
use std::error::Error;

pub fn get_jwc(crawler_config: CrawlerConfig) -> Result<Crawler, Box<dyn Error>> {
    let base_url = "https://jwc.seu.edu.cn".to_string();
    let categories = vec![
        Category {
            label: "最新动态".to_string(),
            path: "/zxdt/list.htm".to_string(),
        },
        Category {
            label: "教务信息".to_string(),
            path: "/jwxx/list.htm".to_string(),
        },
        Category {
            label: "学籍管理".to_string(),
            path: "/xjgl/list.htm".to_string(),
        },
        Category {
            label: "教学研究".to_string(),
            path: "/jxyj/list.htm".to_string(),
        },
        Category {
            label: "实践教学".to_string(),
            path: "/sjjx/list.htm".to_string(),
        },
        Category {
            label: "国际交流".to_string(),
            path: "/gjjl/list.psp".to_string(),
        },
        Category {
            label: "文化素质教育".to_string(),
            path: "/cbxx/list.htm".to_string(),
        },
    ];

    let config = SiteConfig {
        name: "教务处".to_string(),
        base_url,
        categories,
        selectors: SelectionConfig {
            list_row: "#wp_news_w8 table.main tr".to_string(),
            list_title_link: "a[title]".to_string(),
            list_date: "td.main div".to_string(),
            content_body: "div.Article_Content".to_string(),
            current_page: "em.curr_page".to_string(),
            all_pages: "em.all_pages".to_string(),
        },
    };
    Crawler::new(config, crawler_config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jwc_uses_seven_fixed_categories() {
        let crawler_config = CrawlerConfig::bounded(true, 2, 20, 40, 1_000, 4_194_304).unwrap();
        let jwc = get_jwc(crawler_config).unwrap();

        assert_eq!(jwc.site_config.categories.len(), 7);
        assert_eq!(jwc.site_config.base_url, "https://jwc.seu.edu.cn");
    }

    #[test]
    fn detail_fixture_preserves_table_and_controlled_attachment_reference() {
        let crawler_config = CrawlerConfig::bounded(false, 2, 20, 40, 1_000, 4_194_304).unwrap();
        let jwc = get_jwc(crawler_config).unwrap();
        let fixture = include_str!("../../tests/fixtures/jwc_detail.html");

        let content = jwc
            .parse_content_html(
                "https://jwc.seu.edu.cn/2026/0728/c21676a600001/page.htm",
                "div.Article_Content",
                fixture,
            )
            .unwrap();

        assert!(content.text.contains("考试安排正文"));
        assert!(content.text.contains("日期"));
        assert_eq!(
            content.attachment_urls,
            vec!["https://jwc.seu.edu.cn/_upload/file/demo.pdf"]
        );
    }

    #[test]
    fn detail_fixture_reports_selector_change_without_panicking() {
        let crawler_config = CrawlerConfig::bounded(false, 2, 20, 40, 1_000, 4_194_304).unwrap();
        let jwc = get_jwc(crawler_config).unwrap();
        let fixture = include_str!("../../tests/fixtures/jwc_detail.html");

        let error = jwc
            .parse_content_html(
                "https://jwc.seu.edu.cn/2026/0728/c21676a600001/page.htm",
                "div.changed-selector",
                fixture,
            )
            .unwrap_err();

        assert_eq!(error.to_string(), "PARSE_SCHEMA_CHANGED");
    }
}
