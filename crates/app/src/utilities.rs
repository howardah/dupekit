use crate::*;

pub(super) fn selection_by_path(groups: &[DuplicateGroup]) -> BTreeMap<PathBuf, bool> {
    groups
        .iter()
        .flat_map(|group| {
            group
                .files
                .iter()
                .map(|file| (file.path.clone(), group.is_selected(file.id)))
        })
        .collect()
}

/// Restores choices from the preceding result set by path, rather than by the
/// database IDs which are replaced on every scan. New files retain fclones'
/// preferred-directory default. If files have regrouped and the old choices
/// would remove every copy, retain the scanner's default kept copy.
pub(super) fn restore_selection_by_path(
    groups: &mut [DuplicateGroup],
    selections: &BTreeMap<PathBuf, bool>,
) {
    for group in groups {
        let default_kept = group
            .files
            .iter()
            .find(|file| !group.is_selected(file.id))
            .map(|file| file.id);
        let desired = group
            .files
            .iter()
            .filter_map(|file| match selections.get(&file.path) {
                Some(selected) => selected.then_some(file.id),
                // A new file has no former choice, so preserve the scanner's
                // preferred-directory default for it.
                None => group.is_selected(file.id).then_some(file.id),
            })
            .collect::<Vec<_>>();
        let has_known_path = group
            .files
            .iter()
            .any(|file| selections.contains_key(&file.path));
        // A completely new group keeps its scanner default. Unlike that case,
        // an all-false known selection intentionally clears this group.
        if !has_known_path {
            continue;
        }
        let file_ids = group.files.iter().map(|file| file.id).collect::<Vec<_>>();
        for id in file_ids {
            let _ = group.set_selected(id, false);
        }
        let keep = (desired.len() == group.files.len())
            .then_some(default_kept.unwrap_or(group.files[0].id));
        for id in desired {
            if Some(id) != keep {
                let _ = group.set_selected(id, true);
            }
        }
    }
}
pub(super) fn toggle_file(groups: &mut [DuplicateGroup], id: DuplicateFileId) {
    for group in groups {
        if group.files.iter().any(|file| file.id == id) {
            let _ = group.set_selected(id, !group.is_selected(id));
            return;
        }
    }
}
pub(super) fn apply_policy(
    groups: &mut [DuplicateGroup],
    policy: UiPolicy,
    preferred: &[ScanPath],
) {
    let preferred = preferred
        .iter()
        .filter(|p| p.preferred)
        .map(|p| p.path.clone())
        .collect::<Vec<_>>();
    for group in groups {
        group.apply_selection(policy.core(), &preferred);
    }
}
pub(super) fn totals(groups: &[DuplicateGroup]) -> (usize, u64) {
    (
        groups.iter().map(|g| g.selected_ids().len()).sum(),
        groups.iter().map(DuplicateGroup::selected_bytes).sum(),
    )
}
pub(super) fn refresh_history(app: &mut App) {
    match history_items(&app.db) {
        Ok(history) => app.history = history,
        Err(error) => app.notice = Some(format!("Could not refresh scan history: {error}")),
    }
}
pub(super) fn append_notice(app: &mut App, message: String) {
    if let Some(existing) = &mut app.notice {
        existing.push('\n');
        existing.push_str(&message);
    } else {
        app.notice = Some(message);
    }
}
pub(super) fn preflight_failure_message(preflight: &dupekit_core::CleanupPreflight) -> String {
    fn describe(label: &str, paths: &[PathBuf]) -> Option<String> {
        (!paths.is_empty()).then(|| {
            let shown = paths
                .iter()
                .take(4)
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let remainder = paths.len().saturating_sub(4);
            if remainder == 0 {
                format!("{label}: {shown}")
            } else {
                format!("{label}: {shown}, and {remainder} more")
            }
        })
    }

    let details = [
        describe("Missing", &preflight.missing),
        describe("Changed", &preflight.changed),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(". ");
    format!(
        "Cleanup stopped before removing any files because the selected files no longer match the scan. {details}. Refresh the results and review the selection; if this repeats, another application may be updating those files."
    )
}
pub(super) fn history_items(
    db: &Database,
) -> Result<Vec<HistoryItem>, dupekit_storage::StorageError> {
    Ok(db
        .scans()?
        .into_iter()
        .map(|scan| {
            let summary = scan.summary.unwrap_or_default();
            HistoryItem {
                id: scan.id,
                name: scan.name.unwrap_or_else(|| "Untitled scan".into()),
                date: format_scan_time(scan.finished_at.unwrap_or(scan.started_at)),
                status: scan.status,
                groups: summary.duplicate_groups as usize,
                bytes: summary.recoverable_bytes,
            }
        })
        .collect())
}
pub(super) fn format_scan_time(time: std::time::SystemTime) -> String {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => format!("{} UTC", duration.as_secs()),
        Err(_) => "Unknown date".into(),
    }
}
pub(super) fn parse_size_input(value: &str, field: &str) -> Result<Option<u64>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let mut parts = value.split_whitespace();
    let number_text = parts.next().expect("non-empty input has a first token");
    let number = number_text.parse::<f64>().map_err(|_| {
        format!("{field} must be a number followed by an optional unit (for example, 1 MB).")
    })?;
    if !number.is_finite() || number < 0.0 {
        return Err(format!("{field} must be a finite, non-negative size."));
    }
    let unit = parts.next().unwrap_or("B").to_ascii_uppercase();
    if parts.next().is_some() {
        return Err(format!("{field} has unexpected trailing text."));
    }
    let multiplier = match unit.as_str() {
        "B" | "BYTE" | "BYTES" => 1.0,
        "K" | "KB" | "KIB" => 1024.0,
        "M" | "MB" | "MIB" => 1024.0 * 1024.0,
        "G" | "GB" | "GIB" => 1024.0 * 1024.0 * 1024.0,
        "T" | "TB" | "TIB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => {
            return Err(format!(
                "{field} uses an unknown unit. Use B, KB, MB, GB, or TB."
            ));
        }
    };
    let bytes = number * multiplier;
    if !bytes.is_finite() || bytes > u64::MAX as f64 {
        return Err(format!("{field} is too large."));
    }
    Ok(Some(bytes as u64))
}

pub(super) fn size_input_value(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("TB", 1 << 40),
        ("GB", 1 << 30),
        ("MB", 1 << 20),
        ("KB", 1 << 10),
    ];
    for (unit, factor) in UNITS {
        for denominator in [1_u64, 2, 4, 8] {
            let scaled = u128::from(bytes) * u128::from(denominator);
            if scaled % u128::from(factor) == 0 {
                let numerator = scaled / u128::from(factor);
                if denominator == 1 {
                    return format!("{numerator} {unit}");
                }
                let value = numerator as f64 / denominator as f64;
                return format!("{} {unit}", format!("{value:.3}").trim_end_matches('0'));
            }
        }
    }
    format!("{bytes} B")
}
