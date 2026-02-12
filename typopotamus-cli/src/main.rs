use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use comfy_table::{
    Cell, ContentArrangement, Table, modifiers::UTF8_ROUND_CORNERS, presets::UTF8_FULL,
};
use serde::Serialize;
use typopotamus_core::download;
use typopotamus_core::extractor::{extract_fonts_from_url, normalize_target_url};
use typopotamus_core::model::{FontInfo, group_by_family};
use typopotamus_core::selection::{FontSelection, select_font_indices};

#[derive(Debug, Parser)]
#[command(
    name = "typopotamus-cli",
    version,
    about = "Inspect and download web fonts from a website"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Inspect(InspectArgs),
    Download(DownloadArgs),
}

#[derive(Debug, Args)]
struct InspectArgs {
    #[arg(short, long, help = "Website URL to inspect")]
    url: String,

    #[arg(
        long,
        value_name = "FAMILY",
        help = "Limit output to one or more family names (repeatable)",
        num_args = 1..
    )]
    family: Vec<String>,

    #[arg(
        long,
        default_value_t = OutputFormat::Pretty,
        value_enum,
        help = "Output format for inspect results"
    )]
    format: OutputFormat,
}

#[derive(Debug, Args)]
struct DownloadArgs {
    #[arg(short, long, help = "Website URL to inspect and download from")]
    url: String,

    #[arg(
        short,
        long,
        default_value = "downloads",
        help = "Directory where selected fonts are saved"
    )]
    output: PathBuf,

    #[arg(long, help = "Download all discovered fonts")]
    all: bool,

    #[arg(
        long,
        value_name = "FAMILY",
        help = "Select all fonts in a family (repeatable)",
        num_args = 1..
    )]
    family: Vec<String>,

    #[arg(
        long = "font-name",
        value_name = "NAME",
        help = "Select a specific font by name (repeatable)",
        num_args = 1..
    )]
    font_name: Vec<String>,

    #[arg(
        long = "font-url",
        value_name = "URL",
        help = "Select a specific font by URL (repeatable)",
        num_args = 1..
    )]
    font_url: Vec<String>,

    #[arg(
        long,
        value_name = "INDEX",
        help = "Select a font by index from inspect output (repeatable)",
        num_args = 1..
    )]
    index: Vec<usize>,

    #[arg(long, help = "Show selected fonts without downloading")]
    dry_run: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
    Pretty,
    Json,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Inspect(args) => run_inspect(args),
        Commands::Download(args) => run_download(args),
    }
}

fn run_inspect(args: InspectArgs) -> Result<()> {
    let normalized_url = normalize_target_url(&args.url);
    let fonts = extract_fonts_from_url(&normalized_url)
        .with_context(|| format!("failed to extract fonts from {normalized_url}"))?;

    if fonts.is_empty() {
        match args.format {
            OutputFormat::Pretty => println!("No fonts found on {normalized_url}"),
            OutputFormat::Json => {
                let output = InspectOutput {
                    source: normalized_url,
                    total_found: 0,
                    selected_count: 0,
                    family_count: 0,
                    families: Vec::new(),
                };
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
        }
        return Ok(());
    }

    let filtered_indices = if args.family.is_empty() {
        (0..fonts.len()).collect::<Vec<_>>()
    } else {
        let selection = FontSelection {
            all: false,
            families: args.family,
            names: Vec::new(),
            urls: Vec::new(),
            indices: Vec::new(),
        };
        select_font_indices(&fonts, &selection)
    };

    if filtered_indices.is_empty() {
        bail!("no fonts matched requested family filter");
    }

    let output = build_inspect_output(&normalized_url, &fonts, &filtered_indices);

    match args.format {
        OutputFormat::Pretty => print_inspect_summary_pretty(&output),
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&output)?),
    }

    Ok(())
}

fn run_download(args: DownloadArgs) -> Result<()> {
    let normalized_url = normalize_target_url(&args.url);
    let fonts = extract_fonts_from_url(&normalized_url)
        .with_context(|| format!("failed to extract fonts from {normalized_url}"))?;

    if fonts.is_empty() {
        bail!("no fonts were found on {normalized_url}");
    }

    let selection = FontSelection {
        all: args.all,
        families: args.family,
        names: args.font_name,
        urls: args.font_url,
        indices: args.index,
    };

    if !selection.has_selectors() {
        bail!("no selection provided. Use --all or one of --family/--font-name/--font-url/--index");
    }

    let selected_indices = select_font_indices(&fonts, &selection);
    if selected_indices.is_empty() {
        bail!("no fonts matched the provided selectors");
    }

    print_download_selection_pretty(&normalized_url, &fonts, &selected_indices);

    if args.dry_run {
        println!("\nDry run enabled; no files were downloaded.");
        return Ok(());
    }

    let selected_fonts = select_fonts(&fonts, &selected_indices);
    let total = selected_fonts.len();

    eprintln!(
        "\nDownloading {total} fonts into {} ...",
        args.output.display()
    );

    let report = download::download_fonts(&selected_fonts, &args.output, |current, total, font| {
        eprintln!("[{current}/{total}] {}", font.name);
    });

    println!(
        "\nDownloaded {}/{} fonts into {}",
        report.success_count(),
        report.attempted,
        args.output.display()
    );

    if !report.failures.is_empty() {
        eprintln!("{} download(s) failed:", report.failures.len());
        for failure in &report.failures {
            eprintln!("- {failure}");
        }
        bail!("some downloads failed");
    }

    Ok(())
}

fn select_fonts(fonts: &[FontInfo], indices: &[usize]) -> Vec<FontInfo> {
    indices
        .iter()
        .filter_map(|index| fonts.get(*index).cloned())
        .collect()
}

fn print_inspect_summary_pretty(output: &InspectOutput) {
    println!("Source: {}", output.source);
    println!(
        "Selected fonts: {} of {} ({} grouped families)",
        output.selected_count, output.total_found, output.family_count
    );

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header([
            "Family", "Files", "Variants", "Weights", "Styles", "Formats", "Indexes",
        ]);

    for family in &output.families {
        table.add_row([
            Cell::new(&family.name),
            Cell::new(family.files),
            Cell::new(family.variants),
            Cell::new(compact_join(&family.weights, 18)),
            Cell::new(compact_join(&family.styles, 16)),
            Cell::new(compact_join(&family.formats, 14)),
            Cell::new(compact_join(&family.index_ranges, 22)),
        ]);
    }

    println!("\n{table}");
}

fn print_download_selection_pretty(
    source_url: &str,
    fonts: &[FontInfo],
    selected_indices: &[usize],
) {
    let selected: HashSet<usize> = selected_indices.iter().copied().collect();
    let families = group_by_family(fonts);

    println!("Source: {source_url}");
    println!("Selected fonts: {} of {}", selected.len(), fonts.len());

    for family in families {
        let family_indices = family
            .font_indices
            .iter()
            .filter(|index| selected.contains(index))
            .copied()
            .collect::<Vec<_>>();

        if family_indices.is_empty() {
            continue;
        }

        println!("\n{} ({})", family.name, family_indices.len());

        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .apply_modifier(UTF8_ROUND_CORNERS)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(["Index", "Name", "Weight", "Style", "Format", "URL"]);

        for index in family_indices {
            let font = &fonts[index];
            table.add_row([
                Cell::new(index),
                Cell::new(truncate_for_cli(&font.name, 36)),
                Cell::new(&font.weight),
                Cell::new(&font.style),
                Cell::new(&font.format),
                Cell::new(truncate_for_cli(&font.url, 72)),
            ]);
        }

        println!("{table}");
    }
}

fn build_inspect_output(
    source_url: &str,
    fonts: &[FontInfo],
    selected_indices: &[usize],
) -> InspectOutput {
    let mut unique_indices: Vec<usize> = selected_indices
        .iter()
        .copied()
        .filter(|index| *index < fonts.len())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    unique_indices.sort_unstable();

    let mut grouped: BTreeMap<String, FamilyAccumulator> = BTreeMap::new();

    for index in unique_indices {
        let font = &fonts[index];
        let fingerprint = infer_family_fingerprint(font);
        let effective_style = effective_style(font, fingerprint.style_hint.as_deref());
        let effective_weight = effective_weight(font, fingerprint.weight_hint.as_deref());

        let accumulator = grouped
            .entry(fingerprint.key)
            .or_insert_with(|| FamilyAccumulator::new(fingerprint.display));

        accumulator.aliases.insert(font.family.clone());
        accumulator.files += 1;
        accumulator
            .variant_keys
            .insert(format!("{effective_weight}/{effective_style}"));
        accumulator.weights.insert(effective_weight.clone());
        accumulator.styles.insert(effective_style.clone());
        accumulator.formats.insert(font.format.to_ascii_uppercase());
        accumulator.indices.push(index);
        accumulator.fonts.push(FontRowOutput {
            index,
            name: font.name.clone(),
            source_family: font.family.clone(),
            weight: effective_weight,
            style: effective_style,
            format: font.format.clone(),
            url: font.url.clone(),
            referer: font.referer.clone(),
        });
    }

    let mut families = grouped
        .into_values()
        .map(FamilyAccumulator::into_output)
        .collect::<Vec<_>>();

    families.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
    });

    let selected_count = families.iter().map(|family| family.files).sum();

    InspectOutput {
        source: source_url.to_owned(),
        total_found: fonts.len(),
        selected_count,
        family_count: families.len(),
        families,
    }
}

#[derive(Debug)]
struct FamilyFingerprint {
    key: String,
    display: String,
    weight_hint: Option<String>,
    style_hint: Option<String>,
}

fn infer_family_fingerprint(font: &FontInfo) -> FamilyFingerprint {
    let mut tokens = tokenize_source(&font.family);
    cleanup_file_tokens(&mut tokens);
    let (mut weight_hint, mut style_hint) = strip_variant_tokens(&mut tokens);

    if tokens.is_empty() {
        tokens = tokenize_source(&font.name);
        cleanup_file_tokens(&mut tokens);
        let (fallback_weight, fallback_style) = strip_variant_tokens(&mut tokens);
        if weight_hint.is_none() {
            weight_hint = fallback_weight;
        }
        if style_hint.is_none() {
            style_hint = fallback_style;
        }
    }

    if tokens.is_empty() {
        tokens.push("unknown".to_owned());
    }

    let key = tokens.join(" ");
    let display = tokens
        .iter()
        .map(|token| display_token(token))
        .collect::<Vec<_>>()
        .join(" ");

    FamilyFingerprint {
        key,
        display,
        weight_hint,
        style_hint,
    }
}

fn tokenize_source(input: &str) -> Vec<String> {
    let source = strip_known_extension(input);

    let mut tokens = Vec::new();
    let mut chunk = String::new();

    for ch in source.chars() {
        if ch.is_ascii_alphanumeric() {
            chunk.push(ch);
            continue;
        }

        if !chunk.is_empty() {
            tokens.extend(split_camel_chunk(&chunk));
            chunk.clear();
        }
    }

    if !chunk.is_empty() {
        tokens.extend(split_camel_chunk(&chunk));
    }

    tokens
}

fn split_camel_chunk(chunk: &str) -> Vec<String> {
    if chunk.is_empty() {
        return Vec::new();
    }

    let indices = chunk.char_indices().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut start = 0;

    for index in 1..indices.len() {
        let byte_index = indices[index].0;
        let current = indices[index].1;
        let previous = indices[index - 1].1;
        let next = indices.get(index + 1).map(|(_, character)| *character);

        let acronym_to_word_break = current.is_ascii_uppercase()
            && previous.is_ascii_uppercase()
            && next.is_some_and(|character| character.is_ascii_lowercase());

        let lower_to_upper_break = current.is_ascii_uppercase() && previous.is_ascii_lowercase();

        if acronym_to_word_break || lower_to_upper_break {
            let token = chunk[start..byte_index].to_ascii_lowercase();
            if !token.is_empty() {
                tokens.push(token);
            }
            start = byte_index;
        }
    }

    let token = chunk[start..].to_ascii_lowercase();
    if !token.is_empty() {
        tokens.push(token);
    }

    tokens
}

fn strip_known_extension(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    for extension in [".woff2", ".woff", ".ttf", ".otf", ".eot", ".svg"] {
        if lower.ends_with(extension) {
            return input[..input.len() - extension.len()].to_owned();
        }
    }
    input.to_owned()
}

fn cleanup_file_tokens(tokens: &mut Vec<String>) {
    while let Some(last) = tokens.last() {
        if is_hash_token(last) || last == "s" || last == "p" {
            tokens.pop();
        } else {
            break;
        }
    }
}

fn strip_variant_tokens(tokens: &mut Vec<String>) -> (Option<String>, Option<String>) {
    let mut weight_hint = None;
    let mut style_hint = None;

    loop {
        let Some(last) = tokens.last().cloned() else {
            break;
        };

        if style_hint.is_none()
            && let Some(style) = style_hint_from_token(&last)
        {
            style_hint = Some(style);
            tokens.pop();
            continue;
        }

        if weight_hint.is_none()
            && let Some(weight) = weight_hint_from_token(&last)
        {
            weight_hint = Some(weight);
            tokens.pop();
            continue;
        }

        break;
    }

    (weight_hint, style_hint)
}

fn style_hint_from_token(token: &str) -> Option<String> {
    match token {
        "italic" => Some("italic".to_owned()),
        "oblique" => Some("oblique".to_owned()),
        _ => None,
    }
}

fn weight_hint_from_token(token: &str) -> Option<String> {
    match token {
        "thin" => Some("200".to_owned()),
        "extralight" | "ultralight" => Some("100".to_owned()),
        "light" => Some("300".to_owned()),
        "semilight" => Some("300".to_owned()),
        "regular" | "normal" => Some("400".to_owned()),
        "medium" => Some("500".to_owned()),
        "semibold" | "demibold" => Some("600".to_owned()),
        "bold" => Some("700".to_owned()),
        "extrabold" | "ultrabold" | "heavy" => Some("800".to_owned()),
        "black" => Some("900".to_owned()),
        _ => None,
    }
}

fn effective_style(font: &FontInfo, style_hint: Option<&str>) -> String {
    let style = normalize_style(&font.style);
    if style != "normal" {
        return style;
    }

    style_hint.unwrap_or("normal").to_owned()
}

fn effective_weight(font: &FontInfo, weight_hint: Option<&str>) -> String {
    let weight = normalize_weight(&font.weight);
    if weight != "400" {
        return weight;
    }

    weight_hint.unwrap_or("400").to_owned()
}

fn normalize_style(input: &str) -> String {
    let normalized = input.trim().to_ascii_lowercase();
    if normalized.contains("italic") {
        "italic".to_owned()
    } else if normalized.contains("oblique") {
        "oblique".to_owned()
    } else {
        "normal".to_owned()
    }
}

fn normalize_weight(input: &str) -> String {
    let normalized = input.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return "400".to_owned();
    }

    if let Ok(value) = normalized.parse::<u16>() {
        return value.to_string();
    }

    if let Some(mapped) = weight_hint_from_token(&normalized) {
        return mapped;
    }

    if normalized == "normal" {
        "400".to_owned()
    } else {
        normalized
    }
}

fn display_token(token: &str) -> String {
    if token.chars().all(|ch| ch.is_ascii_digit()) {
        return token.to_owned();
    }

    if token.len() <= 2 {
        return token.to_ascii_uppercase();
    }

    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };

    let mut display = String::new();
    display.push(first.to_ascii_uppercase());
    display.push_str(chars.as_str());
    display
}

fn is_hash_token(token: &str) -> bool {
    token.len() >= 6 && token.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn compact_join(values: &[String], max_chars: usize) -> String {
    if values.is_empty() {
        return "-".to_owned();
    }

    let joined = values.join(", ");
    if joined.chars().count() <= max_chars {
        joined
    } else {
        truncate_for_cli(&joined, max_chars)
    }
}

fn truncate_for_cli(input: &str, max_width: usize) -> String {
    if input.chars().count() <= max_width {
        return input.to_owned();
    }

    if max_width <= 3 {
        return ".".repeat(max_width);
    }

    let mut output = String::new();
    for character in input.chars().take(max_width - 3) {
        output.push(character);
    }
    output.push_str("...");
    output
}

fn to_index_ranges(indices: &[usize]) -> Vec<String> {
    if indices.is_empty() {
        return Vec::new();
    }

    let mut ranges = Vec::new();

    let mut start = indices[0];
    let mut previous = indices[0];

    for &current in &indices[1..] {
        if current == previous + 1 {
            previous = current;
            continue;
        }

        ranges.push(format_index_range(start, previous));
        start = current;
        previous = current;
    }

    ranges.push(format_index_range(start, previous));
    ranges
}

fn format_index_range(start: usize, end: usize) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}-{end}")
    }
}

#[derive(Debug)]
struct FamilyAccumulator {
    name: String,
    aliases: BTreeSet<String>,
    files: usize,
    variant_keys: BTreeSet<String>,
    weights: BTreeSet<String>,
    styles: BTreeSet<String>,
    formats: BTreeSet<String>,
    indices: Vec<usize>,
    fonts: Vec<FontRowOutput>,
}

impl FamilyAccumulator {
    fn new(name: String) -> Self {
        Self {
            name,
            aliases: BTreeSet::new(),
            files: 0,
            variant_keys: BTreeSet::new(),
            weights: BTreeSet::new(),
            styles: BTreeSet::new(),
            formats: BTreeSet::new(),
            indices: Vec::new(),
            fonts: Vec::new(),
        }
    }

    fn into_output(mut self) -> FamilyOutput {
        self.indices.sort_unstable();
        self.fonts.sort_by_key(|font| font.index);
        let index_ranges = to_index_ranges(&self.indices);

        FamilyOutput {
            name: self.name,
            aliases: self.aliases.into_iter().collect(),
            files: self.files,
            variants: self.variant_keys.len(),
            weights: self.weights.into_iter().collect(),
            styles: self.styles.into_iter().collect(),
            formats: self.formats.into_iter().collect(),
            indices: self.indices,
            index_ranges,
            fonts: self.fonts,
        }
    }
}

#[derive(Debug, Serialize)]
struct InspectOutput {
    source: String,
    total_found: usize,
    selected_count: usize,
    family_count: usize,
    families: Vec<FamilyOutput>,
}

#[derive(Debug, Serialize)]
struct FamilyOutput {
    name: String,
    aliases: Vec<String>,
    files: usize,
    variants: usize,
    weights: Vec<String>,
    styles: Vec<String>,
    formats: Vec<String>,
    indices: Vec<usize>,
    index_ranges: Vec<String>,
    fonts: Vec<FontRowOutput>,
}

#[derive(Debug, Serialize)]
struct FontRowOutput {
    index: usize,
    name: String,
    source_family: String,
    weight: String,
    style: String,
    format: String,
    url: String,
    referer: String,
}
