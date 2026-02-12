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
                let empty = build_inspect_output(&normalized_url, &fonts, &[]);
                println!("{}", serde_json::to_string_pretty(&empty)?);
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

    match args.format {
        OutputFormat::Pretty => {
            print_grouped_fonts_pretty(&normalized_url, &fonts, &filtered_indices)
        }
        OutputFormat::Json => print_grouped_fonts_json(&normalized_url, &fonts, &filtered_indices)?,
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

    print_grouped_fonts_pretty(&normalized_url, &fonts, &selected_indices);

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

fn print_grouped_fonts_pretty(source_url: &str, fonts: &[FontInfo], selected_indices: &[usize]) {
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
            .set_content_arrangement(ContentArrangement::Dynamic);
        table.set_header(["Index", "Name", "Weight", "Style", "Format", "URL"]);

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

fn print_grouped_fonts_json(
    source_url: &str,
    fonts: &[FontInfo],
    selected_indices: &[usize],
) -> Result<()> {
    let output = build_inspect_output(source_url, fonts, selected_indices);
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn build_inspect_output(
    source_url: &str,
    fonts: &[FontInfo],
    selected_indices: &[usize],
) -> InspectOutput {
    let selected: HashSet<usize> = selected_indices.iter().copied().collect();
    let families = group_by_family(fonts);
    let mut output_families = Vec::new();

    for family in families {
        let mut selected_fonts = Vec::new();
        for index in family.font_indices {
            if !selected.contains(&index) {
                continue;
            }
            let Some(font) = fonts.get(index) else {
                continue;
            };

            selected_fonts.push(FontRowOutput {
                index,
                name: font.name.clone(),
                family: font.family.clone(),
                weight: font.weight.clone(),
                style: font.style.clone(),
                format: font.format.clone(),
                url: font.url.clone(),
                referer: font.referer.clone(),
            });
        }

        if !selected_fonts.is_empty() {
            output_families.push(FamilyOutput {
                name: family.name,
                count: selected_fonts.len(),
                fonts: selected_fonts,
            });
        }
    }

    InspectOutput {
        source: source_url.to_owned(),
        total_found: fonts.len(),
        selected_count: selected.len(),
        families: output_families,
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

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
    Pretty,
    Json,
}

#[derive(Debug, Serialize)]
struct InspectOutput {
    source: String,
    total_found: usize,
    selected_count: usize,
    families: Vec<FamilyOutput>,
}

#[derive(Debug, Serialize)]
struct FamilyOutput {
    name: String,
    count: usize,
    fonts: Vec<FontRowOutput>,
}

#[derive(Debug, Serialize)]
struct FontRowOutput {
    index: usize,
    name: String,
    family: String,
    weight: String,
    style: String,
    format: String,
    url: String,
    referer: String,
}
