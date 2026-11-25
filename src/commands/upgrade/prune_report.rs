//! Best-effort upgrade reporting, using the same read-only prune scanner.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::commands::prune;
use crate::{MemhubError, db};

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Timing {
    BeforeUpgrade,
    AfterUpgrade,
}

impl Timing {
    fn label(self) -> &'static str {
        match self {
            Self::BeforeUpgrade => "before upgrade",
            Self::AfterUpgrade => "after upgrade",
        }
    }
}

#[derive(Serialize)]
pub(super) struct Summary {
    label: String,
    timing: Timing,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    counts: Option<BTreeMap<prune::Category, usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    similarity_warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    review: &'static str,
}

impl Summary {
    pub(super) fn scan(root: &Path, timing: Timing) -> Self {
        let mut summary = Self {
            label: super::abbrev(root),
            timing,
            total: None,
            counts: None,
            backup_warning: None,
            similarity_warning: None,
            error: None,
            review: "run /prune in this repo to review; upgrade takes no prune action",
        };
        // Discovery walks ancestors; never label a parent's scan as this root's.
        let result = db::discover_paths(root).and_then(|paths| {
            if !super::same_file(&paths.repo_root, root) {
                return Err(MemhubError::InvalidInput(format!(
                    "no memhub project at selected root {}",
                    root.display()
                )));
            }
            prune::scan_paths(&paths)
        });
        match result {
            Ok(report) => {
                summary.total = Some(report.total);
                summary.counts = Some(
                    report
                        .counts
                        .into_iter()
                        .map(|(category, count)| (category, count.total))
                        .collect(),
                );
                summary.backup_warning = report.backup_warning;
                summary.similarity_warning = report.similarity_warning;
            }
            Err(e) => summary.error = Some(e.to_string()),
        }
        summary
    }

    pub(super) fn print(&self) {
        let timing = self.timing.label();
        if let Some(counts) = &self.counts {
            let counts = counts
                .iter()
                .map(|(category, count)| format!("{}: {count}", category.heading()))
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "  prune scan ({timing}): {}: {} item(s) ({counts}); {}",
                self.label,
                self.total.unwrap_or_default(),
                self.review
            );
        }
        for warning in [&self.backup_warning, &self.similarity_warning]
            .into_iter()
            .flatten()
        {
            println!(
                "  prune scan ({timing}): {}: warning: {warning}",
                self.label
            );
        }
        if let Some(error) = &self.error {
            println!(
                "  prune scan ({timing}): {}: unavailable ({error}); {}",
                self.label, self.review
            );
        }
    }
}
