use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use comfy_table::{
    Cell, ContentArrangement, Table, modifiers::UTF8_ROUND_CORNERS, presets::UTF8_FULL,
};
use serde::Serialize;
use typopotamus_core::download;
use typopotamus_core::extractor::{extract_fonts_from_url, normalize_target_url};
use typopotamus_core::inspect::{
    InferredFamilyGroup, infer_family_groups, select_indices_by_inferred_family_names,
};
use typopotamus_core::model::FontInfo;
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
        help = "Limit output to one or more family names (matches inferred and source family names)",
        num_args = 1..
    )]
    family: Vec<String>,

    #[arg(
        long,
        default_value_t = InspectView::Family,
        value_enum,
        help = "Inspect grouped families or individual font files"
    )]
    view: InspectView,

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
        help = "Select all fonts in a family (matches inferred and source family names)",
        num_args = 1..
    )]
    family: Vec<String>,

    #[arg(
        long = "font-name",
        value_name = "NAME",
        help = "Select a specific font by file name (repeatable)",
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

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
enum OutputFormat {
    Pretty,
    Json,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
enum InspectView {
    Family,
    Font,
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
        return render_empty_inspect(&normalized_url, args.view, args.format);
    }

    let filtered_indices = if args.family.is_empty() {
        (0..fonts.len()).collect::<Vec<_>>()
    } else {
        select_indices_by_inferred_family_names(&fonts, &args.family)
    };

    if filtered_indices.is_empty() {
        bail!("no fonts matched requested family filter");
    }

    let groups = infer_family_groups(&fonts, &filtered_indices);
    let grouped_output = build_grouped_output(&normalized_url, &fonts, args.view, groups);

    match args.format {
        OutputFormat::Pretty => print_inspect_pretty(&grouped_output),
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&grouped_output)?),
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

    if !has_download_selectors(&args) {
        bail!("no selection provided. Use --all or one of --family/--font-name/--font-url/--index");
    }

    let selected_indices = resolve_download_indices(&fonts, &args);
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

fn has_download_selectors(args: &DownloadArgs) -> bool {
    args.all
        || !args.family.is_empty()
        || !args.font_name.is_empty()
        || !args.font_url.is_empty()
        || !args.index.is_empty()
}

fn resolve_download_indices(fonts: &[FontInfo], args: &DownloadArgs) -> Vec<usize> {
    let mut selected = HashSet::new();

    if args.all {
        selected.extend(0..fonts.len());
    }

    if !args.family.is_empty() {
        let family_indices = select_indices_by_inferred_family_names(fonts, &args.family);
        selected.extend(family_indices);
    }

    let direct_selection = FontSelection {
        all: false,
        families: Vec::new(),
        names: args.font_name.clone(),
        urls: args.font_url.clone(),
        indices: args.index.clone(),
    };
    selected.extend(select_font_indices(fonts, &direct_selection));

    let mut selected_indices = selected.into_iter().collect::<Vec<_>>();
    selected_indices.sort_unstable();
    selected_indices
}

fn render_empty_inspect(source: &str, view: InspectView, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Pretty => {
            println!("No fonts found on {source}");
        }
        OutputFormat::Json => {
            let output = InspectOutput {
                source: source.to_owned(),
                total_found: 0,
                selected_count: 0,
                view,
                family_count: 0,
                families: Vec::new(),
                fonts: Vec::new(),
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
    }

    Ok(())
}

fn print_inspect_pretty(output: &InspectOutput) {
    println!("Source: {}", output.source);
    println!(
        "Selected fonts: {} of {}",
        output.selected_count, output.total_found
    );

    match output.view {
        InspectView::Family => {
            println!("Grouped families: {}", output.family_count);
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
                    Cell::new(compact_join(&family.weights, 20)),
                    Cell::new(compact_join(&family.styles, 18)),
                    Cell::new(compact_join(&family.formats, 14)),
                    Cell::new(compact_join(&family.index_ranges, 24)),
                ]);
            }

            println!("\n{table}");
        }
        InspectView::Font => {
            let mut table = Table::new();
            table
                .load_preset(UTF8_FULL)
                .apply_modifier(UTF8_ROUND_CORNERS)
                .set_content_arrangement(ContentArrangement::Dynamic)
                .set_header([
                    "Index", "Family", "Name", "Weight", "Style", "Format", "URL",
                ]);

            for font in &output.fonts {
                table.add_row([
                    Cell::new(font.index),
                    Cell::new(truncate_for_cli(&font.family, 28)),
                    Cell::new(truncate_for_cli(&font.name, 32)),
                    Cell::new(&font.weight),
                    Cell::new(&font.style),
                    Cell::new(&font.format),
                    Cell::new(truncate_for_cli(&font.url, 76)),
                ]);
            }

            println!("\n{table}");
        }
    }
}

fn print_download_selection_pretty(
    source_url: &str,
    fonts: &[FontInfo],
    selected_indices: &[usize],
) {
    let groups = infer_family_groups(fonts, selected_indices);

    println!("Source: {source_url}");
    println!(
        "Selected fonts: {} of {}",
        selected_indices.len(),
        fonts.len()
    );

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header([
            "Index", "Family", "Name", "Weight", "Style", "Format", "URL",
        ]);

    for group in groups {
        for font in group.fonts {
            table.add_row([
                Cell::new(font.index),
                Cell::new(truncate_for_cli(&group.name, 28)),
                Cell::new(truncate_for_cli(&font.name, 32)),
                Cell::new(font.weight),
                Cell::new(font.style),
                Cell::new(font.format),
                Cell::new(truncate_for_cli(&font.url, 76)),
            ]);
        }
    }

    println!("\n{table}");
}

fn build_grouped_output(
    source_url: &str,
    all_fonts: &[FontInfo],
    view: InspectView,
    groups: Vec<InferredFamilyGroup>,
) -> InspectOutput {
    let selected_count = groups.iter().map(|group| group.files).sum();

    let families = groups
        .iter()
        .map(|group| FamilyOutput {
            key: group.key.clone(),
            name: group.name.clone(),
            aliases: group.aliases.clone(),
            files: group.files,
            variants: group.variants,
            weights: group.weights.clone(),
            styles: group.styles.clone(),
            formats: group.formats.clone(),
            indices: group.font_indices.clone(),
            index_ranges: group.index_ranges.clone(),
        })
        .collect::<Vec<_>>();

    let fonts = groups
        .into_iter()
        .flat_map(|group| {
            group.fonts.into_iter().map(move |font| FontOutput {
                index: font.index,
                family: group.name.clone(),
                source_family: font.source_family,
                name: font.name,
                weight: font.weight,
                style: font.style,
                format: font.format,
                url: font.url,
                referer: font.referer,
            })
        })
        .collect::<Vec<_>>();

    InspectOutput {
        source: source_url.to_owned(),
        total_found: all_fonts.len(),
        selected_count,
        view,
        family_count: families.len(),
        families: if view == InspectView::Family {
            families
        } else {
            Vec::new()
        },
        fonts: if view == InspectView::Font {
            fonts
        } else {
            Vec::new()
        },
    }
}

fn select_fonts(fonts: &[FontInfo], indices: &[usize]) -> Vec<FontInfo> {
    indices
        .iter()
        .filter_map(|index| fonts.get(*index).cloned())
        .collect()
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

#[derive(Debug, Serialize)]
struct InspectOutput {
    source: String,
    total_found: usize,
    selected_count: usize,
    view: InspectView,
    family_count: usize,
    families: Vec<FamilyOutput>,
    fonts: Vec<FontOutput>,
}

#[derive(Debug, Serialize)]
struct FamilyOutput {
    key: String,
    name: String,
    aliases: Vec<String>,
    files: usize,
    variants: usize,
    weights: Vec<String>,
    styles: Vec<String>,
    formats: Vec<String>,
    indices: Vec<usize>,
    index_ranges: Vec<String>,
}

#[derive(Debug, Serialize)]
struct FontOutput {
    index: usize,
    family: String,
    source_family: String,
    name: String,
    weight: String,
    style: String,
    format: String,
    url: String,
    referer: String,
}
