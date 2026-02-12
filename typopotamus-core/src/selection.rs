use std::collections::HashSet;

use crate::model::FontInfo;

#[derive(Clone, Debug, Default)]
pub struct FontSelection {
    pub all: bool,
    pub families: Vec<String>,
    pub names: Vec<String>,
    pub urls: Vec<String>,
    pub indices: Vec<usize>,
}

impl FontSelection {
    pub fn has_selectors(&self) -> bool {
        self.all
            || !self.families.is_empty()
            || !self.names.is_empty()
            || !self.urls.is_empty()
            || !self.indices.is_empty()
    }
}

pub fn select_font_indices(fonts: &[FontInfo], selection: &FontSelection) -> Vec<usize> {
    if selection.all {
        return (0..fonts.len()).collect();
    }

    let family_set: HashSet<String> = selection
        .families
        .iter()
        .map(|value| normalize(value))
        .collect();
    let name_set: HashSet<String> = selection
        .names
        .iter()
        .map(|value| normalize(value))
        .collect();
    let url_set: HashSet<&str> = selection.urls.iter().map(String::as_str).collect();

    let mut selected = HashSet::new();

    for index in &selection.indices {
        if *index < fonts.len() {
            selected.insert(*index);
        }
    }

    for (index, font) in fonts.iter().enumerate() {
        if family_set.contains(&normalize(&font.family))
            || name_set.contains(&normalize(&font.name))
            || url_set.contains(font.url.as_str())
        {
            selected.insert(index);
        }
    }

    let mut sorted = selected.into_iter().collect::<Vec<_>>();
    sorted.sort_unstable();
    sorted
}

fn normalize(input: &str) -> String {
    input.trim().to_ascii_lowercase()
}
