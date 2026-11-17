//! Search results reference the unchanged model; indices are Unicode scalar (not byte) offsets.
use crate::model::{Model, Row};
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct TextMatch {
    pub field: String,
    pub text: String,
    pub indices: Vec<u32>,
    pub score: u32,
}

#[derive(Debug, Serialize)]
pub struct ChildMatch {
    pub child: usize,
    pub matches: Vec<TextMatch>,
}

#[derive(Debug, Serialize)]
pub struct Hit {
    pub group: usize,
    pub row: usize,
    pub tier: u8,
    pub score: u32,
    pub matches: Vec<TextMatch>,
    /// A child appears here iff it matched, allowing expansion/highlighting without filtering the parent away.
    pub children: Vec<ChildMatch>,
}

struct Search {
    pattern: Pattern,
    matcher: Matcher,
    buffer: Vec<char>,
}

impl Search {
    fn field(&mut self, field: &str, text: &str) -> Option<TextMatch> {
        let mut indices = Vec::new();
        let score = self.pattern.indices(
            Utf32Str::new(text, &mut self.buffer),
            &mut self.matcher,
            &mut indices,
        )?;
        indices.sort_unstable();
        indices.dedup();
        Some(TextMatch {
            field: field.into(),
            text: text.into(),
            indices,
            score,
        })
    }

    fn details(&mut self, row: &Row, include_name: bool) -> Vec<TextMatch> {
        let mut matches = Vec::new();
        if include_name && let Some(found) = self.field("name", &row.name) {
            matches.push(found);
        }
        matches.extend(
            row.details
                .iter()
                .enumerate()
                .filter_map(|(index, text)| self.field(&format!("details[{index}]"), text)),
        );
        matches
    }
}

/// Global search ranking (group/row refer back to the two display groups).
/// An empty query preserves normal grouped ordering instead of re-ranking.
pub fn search(model: &Model, query: &str) -> Vec<Hit> {
    if query.trim().is_empty() {
        return model
            .groups
            .iter()
            .enumerate()
            .flat_map(|(group, rows)| {
                rows.rows.iter().enumerate().map(move |(row, _)| Hit {
                    group,
                    row,
                    tier: 1,
                    score: 0,
                    matches: Vec::new(),
                    children: Vec::new(),
                })
            })
            .collect();
    }
    let mut search = Search {
        pattern: Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart),
        matcher: Matcher::new(Config::DEFAULT),
        buffer: Vec::new(),
    };
    let mut hits = Vec::new();
    for (group, rows) in model.groups.iter().enumerate() {
        for (row_index, row) in rows.rows.iter().enumerate() {
            let mut primary = Vec::new();
            for (field, text) in [
                ("name", Some(row.name.as_str())),
                ("branch", row.metadata.branch.as_deref()),
                ("project", row.metadata.project.as_deref()),
            ] {
                if let Some(text) = text
                    && let Some(found) = search.field(field, text)
                {
                    primary.push(found);
                }
            }
            let secondary = search.details(row, false);
            let children: Vec<ChildMatch> = row
                .children
                .iter()
                .enumerate()
                .filter_map(|(index, child)| {
                    let matches = search.details(child, true);
                    (!matches.is_empty()).then_some(ChildMatch {
                        child: index,
                        matches,
                    })
                })
                .collect();
            if primary.is_empty() && secondary.is_empty() && children.is_empty() {
                continue;
            }
            let tier = if primary.is_empty() { 2 } else { 1 };
            let matches = if tier == 1 { primary } else { secondary };
            let score = matches
                .iter()
                .map(|found| found.score)
                .chain(if tier == 2 {
                    children
                        .iter()
                        .flat_map(|child| &child.matches)
                        .map(|found| found.score)
                        .collect()
                } else {
                    Vec::new()
                })
                .max()
                .unwrap_or_default();
            hits.push(Hit {
                group,
                row: row_index,
                tier,
                score,
                matches,
                children,
            });
        }
    }
    hits.sort_by(|a, b| {
        let row_a = &model.groups[a.group].rows[a.row];
        let row_b = &model.groups[b.group].rows[b.row];
        a.tier
            .cmp(&b.tier)
            .then_with(|| row_b.last_viewed.cmp(&row_a.last_viewed))
            .then_with(|| row_b.attention.cmp(&row_a.attention))
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.group.cmp(&b.group))
            .then_with(|| a.row.cmp(&b.row))
    });
    hits
}
