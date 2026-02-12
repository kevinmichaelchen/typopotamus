use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, USER_AGENT};
use scraper::{Html, Selector};
use url::Url;

use crate::model::{FontInfo, sort_fonts};

const MAX_IMPORT_DEPTH: usize = 3;
const HTTP_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/121.0.0.0 Safari/537.36";

static FONT_FACE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)@font-face\s*\{(.*?)\}").expect("valid @font-face regex"));
static IMPORT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?is)@import\s+(?:url\(\s*['"]?([^'\")]+)['"]?\s*\)|['"]([^'"]+)['"])\s*[^;]*;"#)
        .expect("valid @import regex")
});
static SRC_URL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?is)url\(\s*['"]?([^'\")]+)['"]?\s*\)\s*(?:format\(\s*['"]?([^'\")]+)['"]?\s*\))?"#,
    )
    .expect("valid src url regex")
});

pub fn normalize_target_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    }
}

pub fn extract_fonts_from_url(raw_url: &str) -> Result<Vec<FontInfo>> {
    let target_url = Url::parse(raw_url).context("invalid URL")?;
    let client = build_http_client()?;

    let html = fetch_text(&client, &target_url, Some(target_url.as_str()))
        .with_context(|| format!("failed to fetch {}", target_url.as_str()))?;

    let mut fonts = Vec::new();
    let mut visited_css_urls = HashSet::new();

    let document = Html::parse_document(&html);
    let style_selector = Selector::parse("style").expect("valid selector: style");
    let link_selector = Selector::parse("link").expect("valid selector: link");

    for style in document.select(&style_selector) {
        let css = style.text().collect::<Vec<_>>().join("\n");
        let (mut inline_fonts, imports) = parse_css(&css, &target_url, target_url.as_str());
        fonts.append(&mut inline_fonts);
        for import in imports {
            fetch_and_parse_css(
                &client,
                import,
                target_url.as_str(),
                0,
                &mut visited_css_urls,
                &mut fonts,
            );
        }
    }

    let mut initial_css_urls = Vec::new();

    for link in document.select(&link_selector) {
        let rel = link
            .value()
            .attr("rel")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let href = link.value().attr("href").unwrap_or_default();
        let as_attr = link
            .value()
            .attr("as")
            .unwrap_or_default()
            .to_ascii_lowercase();

        if href.is_empty() {
            continue;
        }

        let Some(resolved_url) = resolve_url(&target_url, href) else {
            continue;
        };

        let is_stylesheet = rel.split_whitespace().any(|token| token == "stylesheet");
        let is_preload = rel.split_whitespace().any(|token| token == "preload");
        let is_prefetch = rel.split_whitespace().any(|token| token == "prefetch");

        if is_stylesheet || (is_preload && as_attr == "style") {
            initial_css_urls.push(resolved_url);
        } else if (is_preload || is_prefetch) && as_attr == "font" {
            let name =
                file_name_from_url(&resolved_url).unwrap_or_else(|| "preloaded-font".to_owned());
            let family = family_from_name(&name);
            fonts.push(FontInfo {
                name,
                family,
                format: format_from_url(&resolved_url),
                url: resolved_url,
                weight: "400".to_owned(),
                style: "normal".to_owned(),
                referer: target_url.as_str().to_owned(),
            });
        }
    }

    for css_url in initial_css_urls {
        if let Ok(parsed_css_url) = Url::parse(&css_url) {
            fetch_and_parse_css(
                &client,
                parsed_css_url,
                target_url.as_str(),
                0,
                &mut visited_css_urls,
                &mut fonts,
            );
        }
    }

    dedupe_fonts(&mut fonts);
    sort_fonts(&mut fonts);

    Ok(fonts)
}

fn build_http_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .context("failed to create HTTP client")
}

fn fetch_and_parse_css(
    client: &Client,
    css_url: Url,
    referer: &str,
    depth: usize,
    visited: &mut HashSet<String>,
    out_fonts: &mut Vec<FontInfo>,
) {
    if depth > MAX_IMPORT_DEPTH || !visited.insert(css_url.to_string()) {
        return;
    }

    let Ok(css) = fetch_text(client, &css_url, Some(referer)) else {
        return;
    };

    let (mut parsed_fonts, imports) = parse_css(&css, &css_url, referer);
    out_fonts.append(&mut parsed_fonts);

    for import in imports {
        fetch_and_parse_css(client, import, referer, depth + 1, visited, out_fonts);
    }
}

fn fetch_text(client: &Client, url: &Url, referer: Option<&str>) -> Result<String> {
    let mut request = client
        .get(url.as_str())
        .header(USER_AGENT, HTTP_USER_AGENT)
        .header(
            ACCEPT,
            "text/html,application/xhtml+xml,application/xml;q=0.9,text/css,*/*;q=0.8",
        );

    if let Some(referer_header) = referer {
        request = request.header("Referer", referer_header);
    }

    let response = request.send()?;
    if !response.status().is_success() {
        anyhow::bail!("request failed with status {}", response.status());
    }

    response.text().context("failed reading response body")
}

fn parse_css(css: &str, base_url: &Url, referer: &str) -> (Vec<FontInfo>, Vec<Url>) {
    let mut fonts = Vec::new();
    let mut imports = Vec::new();

    for capture in IMPORT_RE.captures_iter(css) {
        let raw_import = capture
            .get(1)
            .or_else(|| capture.get(2))
            .map(|m| m.as_str())
            .unwrap_or_default();

        if let Some(url) = resolve_url_to_url(base_url, raw_import) {
            imports.push(url);
        }
    }

    for capture in FONT_FACE_RE.captures_iter(css) {
        let block = capture.get(1).map(|m| m.as_str()).unwrap_or_default();
        let declarations = parse_css_declarations(block);

        let Some(family_raw) = declarations.get("font-family") else {
            continue;
        };
        let Some(src_raw) = declarations.get("src") else {
            continue;
        };

        let family = normalize_family_name(family_raw);
        if family.is_empty() {
            continue;
        }

        let Some(best_source) = pick_best_source(src_raw, base_url) else {
            continue;
        };

        let name = if best_source.url.starts_with("data:") {
            format!("{}-embedded", slug_for_file_name(&family))
        } else {
            file_name_from_url(&best_source.url).unwrap_or_else(|| {
                format!("{}-{}", slug_for_file_name(&family), best_source.format)
            })
        };

        let weight = declarations
            .get("font-weight")
            .cloned()
            .unwrap_or_else(|| "400".to_owned());
        let style = declarations
            .get("font-style")
            .cloned()
            .unwrap_or_else(|| "normal".to_owned());

        fonts.push(FontInfo {
            name,
            family,
            format: best_source.format,
            url: best_source.url,
            weight,
            style,
            referer: referer.to_owned(),
        });
    }

    (fonts, imports)
}

fn parse_css_declarations(block: &str) -> HashMap<String, String> {
    let mut declarations = HashMap::new();
    let mut current = String::new();
    let mut paren_depth = 0_i32;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;

    for ch in block.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        if ch == '\\' {
            current.push(ch);
            escaped = true;
            continue;
        }

        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
        } else if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
        } else if !in_single_quote && !in_double_quote {
            if ch == '(' {
                paren_depth += 1;
            } else if ch == ')' {
                paren_depth = (paren_depth - 1).max(0);
            }
        }

        if ch == ';' && paren_depth == 0 && !in_single_quote && !in_double_quote {
            push_declaration(&mut declarations, &current);
            current.clear();
            continue;
        }

        current.push(ch);
    }

    push_declaration(&mut declarations, &current);

    declarations
}

fn push_declaration(declarations: &mut HashMap<String, String>, raw_declaration: &str) {
    let trimmed = raw_declaration.trim();
    if trimmed.is_empty() {
        return;
    }

    let Some((name, value)) = trimmed.split_once(':') else {
        return;
    };

    declarations.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
}

#[derive(Debug)]
struct SourceCandidate {
    url: String,
    format: String,
}

fn pick_best_source(src_value: &str, base_url: &Url) -> Option<SourceCandidate> {
    let mut candidates = Vec::new();

    for capture in SRC_URL_RE.captures_iter(src_value) {
        let raw_url = capture
            .get(1)
            .map(|m| m.as_str().trim())
            .unwrap_or_default();
        if raw_url.is_empty() {
            continue;
        }

        let Some(resolved_url) = resolve_url(base_url, raw_url) else {
            continue;
        };

        let format = capture
            .get(2)
            .map(|m| m.as_str().trim().to_ascii_uppercase())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format_from_url(raw_url));

        candidates.push(SourceCandidate {
            url: resolved_url,
            format,
        });
    }

    if candidates.is_empty() {
        return None;
    }

    candidates.sort_by_key(|candidate| format_rank(&candidate.format));
    candidates.into_iter().next()
}

fn format_rank(format: &str) -> usize {
    match format.trim().to_ascii_uppercase().as_str() {
        "WOFF2" => 0,
        "WOFF" => 1,
        "OPENTYPE" | "OTF" => 2,
        "TRUETYPE" | "TTF" => 3,
        "EOT" => 4,
        "SVG" => 5,
        _ => 6,
    }
}

fn normalize_family_name(raw: &str) -> String {
    raw.trim().trim_matches('"').trim_matches('\'').to_owned()
}

fn resolve_url(base: &Url, raw: &str) -> Option<String> {
    if raw.starts_with("data:") {
        return Some(raw.to_owned());
    }

    if let Ok(parsed) = Url::parse(raw) {
        return Some(parsed.to_string());
    }

    base.join(raw).ok().map(|joined| joined.to_string())
}

fn resolve_url_to_url(base: &Url, raw: &str) -> Option<Url> {
    if raw.starts_with("data:") {
        return None;
    }

    if let Ok(parsed) = Url::parse(raw) {
        return Some(parsed);
    }

    base.join(raw).ok()
}

fn format_from_url(url: &str) -> String {
    let clean_url = url.split(['?', '#']).next().unwrap_or(url);
    let extension = clean_url
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();

    match extension.as_str() {
        "woff2" => "WOFF2",
        "woff" => "WOFF",
        "ttf" => "TRUETYPE",
        "otf" => "OPENTYPE",
        "eot" => "EOT",
        "svg" => "SVG",
        _ => "UNKNOWN",
    }
    .to_owned()
}

fn file_name_from_url(url: &str) -> Option<String> {
    if url.starts_with("data:") {
        return None;
    }

    let parsed = Url::parse(url).ok()?;
    let segment = parsed.path_segments()?.next_back()?;
    if segment.is_empty() {
        None
    } else {
        Some(segment.to_owned())
    }
}

fn family_from_name(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(base, _)| base)
        .unwrap_or(name)
        .to_owned()
}

fn dedupe_fonts(fonts: &mut Vec<FontInfo>) {
    let mut seen = HashSet::new();
    fonts.retain(|font| seen.insert(font.url.clone()));
}

fn slug_for_file_name(input: &str) -> String {
    let mut value = String::with_capacity(input.len());
    let mut previous_was_separator = false;

    for character in input.chars() {
        if character.is_ascii_alphanumeric() {
            value.push(character.to_ascii_lowercase());
            previous_was_separator = false;
        } else if !previous_was_separator {
            value.push('-');
            previous_was_separator = true;
        }
    }

    value.trim_matches('-').to_owned()
}
