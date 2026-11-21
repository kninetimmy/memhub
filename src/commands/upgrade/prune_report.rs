//! Best-effort upgrade reporting, using the same read-only prune scanner.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::commands::prune;

#[derive(Serialize)]
pub(super) struct Summary {
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    counts: Option<BTreeMap<prune::Category, usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    review: &'static str,
}

impl Summary {
    pub(super) fn scan(root: &Path) -> Self {
        Self::from_report(root, prune::scan(root))
    }

    pub(super) fn preview(root: &Path, files: &prune::UpgradeFiles) -> Self {
        Self::from_report(root, prune::preview_upgrade(root, files))
    }

    fn from_report(root: &Path, result: crate::Result<prune::Report>) -> Self {
        let mut summary = Self {
            label: super::abbrev(root),
            total: None,
            counts: None,
            warning: None,
            error: None,
            review: "run /prune in this repo to review; upgrade takes no prune action",
        };
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
                summary.warning = report.backup_warning;
            }
            Err(e) => summary.error = Some(e.to_string()),
        }
        summary
    }

    pub(super) fn print(&self) {
        if let Some(counts) = &self.counts {
            let counts = counts
                .iter()
                .map(|(category, count)| format!("{}: {count}", category.heading()))
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "  prune scan: {}: {} item(s) ({counts}); {}",
                self.label,
                self.total.unwrap_or_default(),
                self.review
            );
        }
        if let Some(warning) = &self.warning {
            println!("  prune scan: {}: warning: {warning}", self.label);
        }
        if let Some(error) = &self.error {
            println!(
                "  prune scan: {}: warning: scan failed ({error}); {}",
                self.label, self.review
            );
        }
    }
}
