use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::model::{FontFamily, FontInfo};

#[derive(Clone, Debug)]
pub struct InferredFontEntry {
    pub index: usize,
    pub name: String,
    pub source_family: String,
    pub weight: String,
    pub style: String,
    pub format: String,
    pub url: String,
    pub referer: String,
}

#[derive(Clone, Debug)]
pub struct InferredFamilyGroup {
    pub key: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub files: usize,
    pub variants: usize,
    pub weights: Vec<String>,
    pub styles: Vec<String>,
    pub formats: Vec<String>,
    pub font_indices: Vec<usize>,
    pub index_ranges: Vec<String>,
    pub fonts: Vec<InferredFontEntry>,
}

#[derive(Debug)]
struct FamilyFingerprint {
    key: String,
    display: String,
    weight_hint: Option<String>,
    style_hint: Option<String>,
}

#[derive(Debug)]
struct FamilyAccumulator {
    key: String,
    name: String,
    aliases: BTreeSet<String>,
    files: usize,
    variant_keys: BTreeSet<String>,
    weights: BTreeSet<String>,
    styles: BTreeSet<String>,
    formats: BTreeSet<String>,
    indices: Vec<usize>,
    fonts: Vec<InferredFontEntry>,
}

impl FamilyAccumulator {
    fn new(key: String, name: String) -> Self {
        Self {
            key,
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

    fn into_output(mut self) -> InferredFamilyGroup {
        self.indices.sort_unstable();
        self.fonts.sort_by_key(|font| font.index);
        let index_ranges = to_index_ranges(&self.indices);

        InferredFamilyGroup {
            key: self.key,
            name: self.name,
            aliases: self.aliases.into_iter().collect(),
            files: self.files,
            variants: self.variant_keys.len(),
            weights: self.weights.into_iter().collect(),
            styles: self.styles.into_iter().collect(),
            formats: self.formats.into_iter().collect(),
            font_indices: self.indices,
            index_ranges,
            fonts: self.fonts,
        }
    }
}

pub fn infer_family_groups_all(fonts: &[FontInfo]) -> Vec<InferredFamilyGroup> {
    let all_indices = (0..fonts.len()).collect::<Vec<_>>();
    infer_family_groups(fonts, &all_indices)
}

pub fn infer_family_groups(
    fonts: &[FontInfo],
    selected_indices: &[usize],
) -> Vec<InferredFamilyGroup> {
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
            .entry(fingerprint.key.clone())
            .or_insert_with(|| FamilyAccumulator::new(fingerprint.key, fingerprint.display));

        accumulator.aliases.insert(font.family.clone());
        accumulator.files += 1;
        accumulator
            .variant_keys
            .insert(format!("{effective_weight}/{effective_style}"));
        accumulator.weights.insert(effective_weight.clone());
        accumulator.styles.insert(effective_style.clone());
        accumulator.formats.insert(font.format.to_ascii_uppercase());
        accumulator.indices.push(index);
        accumulator.fonts.push(InferredFontEntry {
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
            .then_with(|| a.key.cmp(&b.key))
    });

    families
}

pub fn group_by_inferred_family(fonts: &[FontInfo]) -> Vec<FontFamily> {
    infer_family_groups_all(fonts)
        .into_iter()
        .map(|family| FontFamily {
            name: family.name,
            font_indices: family.font_indices,
        })
        .collect()
}

pub fn select_indices_by_inferred_family_names(
    fonts: &[FontInfo],
    family_names: &[String],
) -> Vec<usize> {
    if family_names.is_empty() {
        return Vec::new();
    }

    let requested = family_names
        .iter()
        .map(|name| normalize(name))
        .collect::<HashSet<_>>();

    let groups = infer_family_groups_all(fonts);
    let mut selected = HashSet::new();

    for group in groups {
        let mut matches = requested.contains(&normalize(&group.name));
        if !matches {
            matches = group
                .aliases
                .iter()
                .any(|alias| requested.contains(&normalize(alias)));
        }

        if matches {
            for index in group.font_indices {
                selected.insert(index);
            }
        }
    }

    let mut indices = selected.into_iter().collect::<Vec<_>>();
    indices.sort_unstable();
    indices
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

fn normalize(input: &str) -> String {
    input.trim().to_ascii_lowercase()
}
