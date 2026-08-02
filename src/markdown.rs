use htmd::HtmlToMarkdown;
use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use url::Url;

pub(crate) fn get_pretty_text(
    element: ElementRef,
    base_url: &Url,
    keep_complex_tables: bool,
) -> Result<String, &'static str> {
    let html_fragment = element.html();
    let pre_cleaned_html = html_fragment.replace("&nbsp;", " ").replace("&#160;", " ");

    let (input_html, table_replacements) = if keep_complex_tables {
        let (html, replacements) = replace_complex_tables_with_placeholders(&pre_cleaned_html);
        (html, Some(replacements))
    } else {
        (pre_cleaned_html, None)
    };

    let converter = HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style", "colgroup", "col"])
        .build();
    let raw_markdown = converter
        .convert(&input_html)
        .map_err(|_| "MARKDOWN_CONVERSION_FAILED")?;

    let cleaned = fix_markdown_links(&raw_markdown, base_url).replace("||", "|\n|");
    let re_multi_spaces = Regex::new(r"[ \t]{2,}").unwrap();

    let lines: Vec<String> = cleaned
        .lines()
        .map(|line| {
            let t = line.trim().replace("&nbsp;", " ").replace("\u{a0}", " ");
            re_multi_spaces.replace_all(&t, " ").to_string()
        })
        .filter(|line| !line.is_empty())
        .collect();

    let mut result = String::new();
    for i in 0..lines.len() {
        result.push_str(&lines[i]);
        if i + 1 < lines.len() {
            if lines[i].starts_with('|') && lines[i + 1].starts_with('|') {
                result.push('\n');
            } else {
                result.push_str("\n\n");
            }
        }
    }

    let mut final_markdown = clean_markdown(&fix_markdown_table_separator(&result));

    if let Some(replacements) = table_replacements {
        for (placeholder, table_html) in &replacements {
            final_markdown = final_markdown.replace(placeholder, table_html);
        }
    }

    Ok(final_markdown)
}

fn replace_complex_tables_with_placeholders(html: &str) -> (String, Vec<(String, String)>) {
    let document = Html::parse_document(html);
    let mut replacements: Vec<(String, String)> = Vec::new();
    let mut placeholder_index = 0;

    let table_sel = Selector::parse("table").unwrap();
    let td_th_sel = Selector::parse("td, th").unwrap();

    let mut work_html = html.to_string();

    for table in document.select(&table_sel) {
        let mut has_complex_cell = false;
        for cell in table.select(&td_th_sel) {
            if cell.value().attr("rowspan").is_some() || cell.value().attr("colspan").is_some() {
                has_complex_cell = true;
                break;
            }
        }

        if has_complex_cell {
            let table_html = table.html();
            let placeholder = format!("HTMLTABLEPLACEHOLDER{}", placeholder_index);
            let cleaned_table = clean_html_table(&table_html);
            replacements.push((placeholder.clone(), cleaned_table));

            work_html = work_html.replace(&table_html, &placeholder);
            placeholder_index += 1;
        }
    }

    (work_html, replacements)
}

fn clean_html_table(html: &str) -> String {
    // 删除 img 标签（图标）
    let re_img = Regex::new(r"<img[^>]*>").unwrap();
    let result = re_img.replace_all(html, "");

    let allowed_attrs = [
        "rowspan", "colspan", "valign", "align", "href", "src", "alt", "title", "width", "height",
    ];

    let re_attr = Regex::new(r#"(\w+)=["'][^"']*["']"#).unwrap();

    let result = re_attr.replace_all(&result, |caps: &regex::Captures| {
        let attr_name = &caps[1];
        if allowed_attrs.contains(&attr_name) {
            caps[0].to_string()
        } else {
            String::new()
        }
    });

    let re_empty_attrs = Regex::new(r#"\s+\w+=""|\w+=""\s+"#).unwrap();
    let result = re_empty_attrs.replace_all(&result, " ").to_string();

    result.trim().to_string()
}

fn fix_markdown_links(md: &str, base_url: &Url) -> String {
    let re = Regex::new(r"(?P<p>!?\[.*?])\((?P<u>[^ )]+)(?:\s+.*?)?\)").unwrap();
    re.replace_all(md, |caps: &regex::Captures| {
        let prefix = &caps["p"];
        let link = &caps["u"];
        if let Ok(absolute_url) = base_url.join(link) {
            let url_str = absolute_url.to_string();
            if url_str.contains("icon_") {
                return "".to_string();
            }
            format!("{}({})", prefix, url_str)
        } else {
            format!("{}({})", prefix, link)
        }
    })
    .to_string()
}

fn fix_markdown_table_separator(md: &str) -> String {
    let mut lines: Vec<String> = md.lines().map(|s| s.to_string()).collect();
    if lines.len() < 2 {
        return md.to_string();
    }

    let header_indices: Vec<usize> = lines
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| {
            !pair[0].trim().is_empty()
                && pair[1].trim().starts_with('|')
                && !pair[1].trim().starts_with("|--")
        })
        .map(|(i, _)| i)
        .collect();

    for header_idx in header_indices.into_iter().rev() {
        // 逆序处理，避免索引偏移
        let column_count = lines[header_idx].matches('|').count().saturating_sub(1);
        if column_count > 0
            && header_idx + 1 < lines.len()
            && !lines[header_idx + 1].contains("---")
        {
            let separator = format!("| {} |", vec!["---"; column_count].join(" | "));
            lines.insert(header_idx + 1, separator);
        }
    }

    lines.join("\n")
}

fn clean_markdown(markdown: &str) -> String {
    let re_extra_asterisks = Regex::new(r"\*{4}").unwrap();

    let result = remove_empty_bold_pairs(markdown);
    let result = re_extra_asterisks.replace_all(&result, "");

    result.to_string()
}

fn is_punctuation(c: char) -> bool {
    c.is_ascii_punctuation()
        || matches!(
            c,
            '，' | '。'
                | '！'
                | '？'
                | '；'
                | '：'
                | '"'
                | '\''
                | '（'
                | '）'
                | '【'
                | '】'
                | '《'
                | '》'
                | '…'
                | '、'
        )
}

fn remove_empty_bold_pairs(md: &str) -> String {
    let chars: Vec<char> = md.chars().collect();
    let mut result = String::new();
    let mut i = 0;

    while i < chars.len() {
        if i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '*' {
            i += 2;

            let mut temp = String::new();
            let mut found_end = false;

            while i + 1 < chars.len() {
                if chars[i] == '*' && chars[i + 1] == '*' {
                    found_end = true;
                    i += 2;
                    break;
                }
                temp.push(chars[i]);
                i += 1;
            }

            if found_end && temp.chars().all(|c| c.is_whitespace()) {
                continue;
            } else if found_end {
                result.push_str("**");
                result.push_str(&temp);
                result.push_str("**");

                if i < chars.len() {
                    let next_char = chars[i];
                    if !next_char.is_whitespace() && !is_punctuation(next_char) {
                        result.push(' ');
                    }
                }
            } else {
                result.push_str("**");
                result.push_str(&temp);
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}
