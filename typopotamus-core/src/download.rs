use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use percent_encoding::percent_decode_str;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, CONTENT_TYPE, ORIGIN, REFERER, USER_AGENT};
use url::Url;

use crate::model::FontInfo;

const HTTP_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/121.0.0.0 Safari/537.36";

#[derive(Debug, Default)]
pub struct DownloadReport {
    pub attempted: usize,
    pub saved_files: Vec<PathBuf>,
    pub failures: Vec<String>,
}

impl DownloadReport {
    pub fn success_count(&self) -> usize {
        self.saved_files.len()
    }
}

pub fn download_fonts<F>(
    fonts: &[FontInfo],
    output_root: &Path,
    mut on_progress: F,
) -> DownloadReport
where
    F: FnMut(usize, usize, &FontInfo),
{
    let mut report = DownloadReport {
        attempted: fonts.len(),
        ..DownloadReport::default()
    };

    if let Err(error) = fs::create_dir_all(output_root) {
        report.failures.push(format!(
            "could not create output directory {}: {error}",
            output_root.display()
        ));
        return report;
    }

    let client = match build_http_client() {
        Ok(client) => client,
        Err(error) => {
            report
                .failures
                .push(format!("could not create HTTP client: {error}"));
            return report;
        }
    };

    let mut used_paths = HashSet::new();

    for (index, font) in fonts.iter().enumerate() {
        on_progress(index + 1, fonts.len(), font);

        match download_single_font(&client, font, output_root, &mut used_paths) {
            Ok(saved_path) => report.saved_files.push(saved_path),
            Err(error) => report
                .failures
                .push(format!("{} ({}) -> {error}", font.name, font.url)),
        }
    }

    report
}

fn build_http_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(45))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .context("failed to create HTTP client")
}

fn download_single_font(
    client: &Client,
    font: &FontInfo,
    output_root: &Path,
    used_paths: &mut HashSet<PathBuf>,
) -> Result<PathBuf> {
    let (bytes, mime_type) = if font.url.starts_with("data:") {
        decode_data_url(&font.url)?
    } else {
        fetch_remote_font(client, font)?
    };

    let extension = extension_for_font(font, mime_type.as_deref());
    let family_dir = output_root.join(sanitize_component(&font.family));
    fs::create_dir_all(&family_dir)
        .with_context(|| format!("failed to create family directory {}", family_dir.display()))?;

    let stem = file_stem_for_font(font);
    let file_path = unique_output_path(&family_dir, &stem, extension, used_paths);

    fs::write(&file_path, bytes)
        .with_context(|| format!("failed writing file {}", file_path.display()))?;

    Ok(file_path)
}

fn fetch_remote_font(client: &Client, font: &FontInfo) -> Result<(Vec<u8>, Option<String>)> {
    let mut request = client
        .get(&font.url)
        .header(USER_AGENT, HTTP_USER_AGENT)
        .header(ACCEPT, "*/*");

    if !font.referer.is_empty() {
        request = request.header(REFERER, &font.referer);
        if let Ok(parsed_referer) = Url::parse(&font.referer) {
            request = request.header(ORIGIN, parsed_referer.origin().ascii_serialization());
        }
    }

    let response = request.send().context("request failed")?;
    if !response.status().is_success() {
        anyhow::bail!("HTTP {}", response.status());
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_owned());

    let bytes = response.bytes().context("failed to read response bytes")?;
    Ok((bytes.to_vec(), content_type))
}

fn decode_data_url(input: &str) -> Result<(Vec<u8>, Option<String>)> {
    let payload = input
        .strip_prefix("data:")
        .context("invalid data URL: missing data: prefix")?;
    let (meta, data) = payload
        .split_once(',')
        .context("invalid data URL: missing comma separator")?;

    let is_base64 = meta
        .split(';')
        .any(|segment| segment.eq_ignore_ascii_case("base64"));
    let mime_type = meta
        .split(';')
        .next()
        .filter(|value| !value.is_empty())
        .map(|value| value.to_owned());

    let bytes = if is_base64 {
        STANDARD
            .decode(data.trim())
            .context("failed to decode base64 font bytes")?
    } else {
        percent_decode_str(data).collect::<Vec<u8>>()
    };

    Ok((bytes, mime_type))
}

fn extension_for_font(font: &FontInfo, content_type: Option<&str>) -> &'static str {
    let format = font.format.to_ascii_uppercase();
    match format.as_str() {
        "WOFF2" => "woff2",
        "WOFF" => "woff",
        "OPENTYPE" | "OTF" => "otf",
        "TRUETYPE" | "TTF" => "ttf",
        "EOT" => "eot",
        "SVG" => "svg",
        _ => {
            if let Some(mime) = content_type {
                if mime.contains("woff2") {
                    return "woff2";
                }
                if mime.contains("woff") {
                    return "woff";
                }
                if mime.contains("opentype") || mime.contains("otf") {
                    return "otf";
                }
                if mime.contains("truetype") || mime.contains("ttf") {
                    return "ttf";
                }
            }
            "bin"
        }
    }
}

fn file_stem_for_font(font: &FontInfo) -> String {
    let base_name = strip_extension(&font.name);
    let normalized_base = sanitize_component(&base_name);
    let normalized_weight = sanitize_component(&font.weight);
    let normalized_style = sanitize_component(&font.style);

    let mut stem = String::new();
    if !normalized_base.is_empty() {
        stem.push_str(&normalized_base);
    } else {
        stem.push_str("font");
    }

    if !normalized_weight.is_empty() {
        stem.push('-');
        stem.push_str(&normalized_weight);
    }

    if !normalized_style.is_empty() {
        stem.push('-');
        stem.push_str(&normalized_style);
    }

    stem
}

fn unique_output_path(
    directory: &Path,
    stem: &str,
    extension: &str,
    used_paths: &mut HashSet<PathBuf>,
) -> PathBuf {
    let normalized_stem = if stem.is_empty() { "font" } else { stem };

    for attempt in 0_u32.. {
        let file_name = if attempt == 0 {
            format!("{normalized_stem}.{extension}")
        } else {
            format!("{normalized_stem}-{attempt}.{extension}")
        };

        let candidate = directory.join(file_name);
        if !candidate.exists() && used_paths.insert(candidate.clone()) {
            return candidate;
        }
    }

    unreachable!("u32 range is effectively unbounded for filename conflict attempts")
}

fn strip_extension(name: &str) -> String {
    Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.to_owned())
        .unwrap_or_else(|| name.to_owned())
}

fn sanitize_component(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut previous_was_separator = false;

    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            output.push(character.to_ascii_lowercase());
            previous_was_separator = false;
        } else if !previous_was_separator {
            output.push('-');
            previous_was_separator = true;
        }
    }

    output.trim_matches('-').to_owned()
}
