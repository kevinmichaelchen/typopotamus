use std::cmp::Ordering;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FontInfo {
    pub name: String,
    pub family: String,
    pub format: String,
    pub url: String,
    pub weight: String,
    pub style: String,
    pub referer: String,
}

#[derive(Clone, Debug)]
pub struct FontFamily {
    pub name: String,
    pub font_indices: Vec<usize>,
}

pub fn sort_fonts(fonts: &mut [FontInfo]) {
    fonts.sort_by(compare_fonts);
}

pub fn group_by_family(fonts: &[FontInfo]) -> Vec<FontFamily> {
    let mut grouped: BTreeMap<String, Vec<usize>> = BTreeMap::new();

    for (index, font) in fonts.iter().enumerate() {
        grouped.entry(font.family.clone()).or_default().push(index);
    }

    grouped
        .into_iter()
        .map(|(name, font_indices)| FontFamily { name, font_indices })
        .collect()
}

fn compare_fonts(a: &FontInfo, b: &FontInfo) -> Ordering {
    let family_cmp = a
        .family
        .to_ascii_lowercase()
        .cmp(&b.family.to_ascii_lowercase());
    if family_cmp != Ordering::Equal {
        return family_cmp;
    }

    let style_cmp = is_italic(&a.style).cmp(&is_italic(&b.style));
    if style_cmp != Ordering::Equal {
        return style_cmp;
    }

    let weight_cmp = (weight_value(&a.weight) - 400)
        .abs()
        .cmp(&(weight_value(&b.weight) - 400).abs());
    if weight_cmp != Ordering::Equal {
        return weight_cmp;
    }

    let name_cmp = a
        .name
        .to_ascii_lowercase()
        .cmp(&b.name.to_ascii_lowercase());
    if name_cmp != Ordering::Equal {
        return name_cmp;
    }

    a.url.cmp(&b.url)
}

fn is_italic(style: &str) -> u8 {
    if style.to_ascii_lowercase().contains("italic") {
        1
    } else {
        0
    }
}

fn weight_value(weight: &str) -> i32 {
    let normalized = weight.trim().to_ascii_lowercase();
    if let Ok(value) = normalized.parse::<i32>() {
        return value;
    }

    if normalized.contains("bold") {
        700
    } else {
        400
    }
}
