// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

//! The request-table column widths (M7): the header row carries a draggable
//! divider between each pair of columns; dragging a divider resizes the
//! adjacent columns (the total width stays constant), every column has a
//! minimum width, and the dividers cannot be dragged past those limits. The
//! widths are persisted in the config together with the window width they
//! were saved at and are restored on startup only when the window width is
//! unchanged; otherwise the defaults are used.
//!
//! The `Model` column is the flex column: its width is always the remainder
//! that makes the columns fill the table at the current window width
//! (`rebalance`). Dragging a divider next to it resizes the fixed column and
//! the `Model` column absorbs the difference.
//!
//! When the window opens maximized the width check is skipped: the pre-`run`
//! width is the last normal (or default) width, not the maximized width the
//! saved widths may have been recorded at, so it cannot be compared with the
//! saved width — the caller rebalances the `Model` column to the actual width
//! right after, so the saved widths are safe to restore.

use serde::Serialize;

/// A request-table column, in the header row's order (M7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Id,
    Time,
    Model,
    Prompt,
    Compl,
    Think,
    Ttft,
    Total,
    Reason,
    Status,
}

impl Column {
    /// The number of table columns.
    pub const COUNT: usize = 10;
    /// All columns, in the header row's order.
    pub const ALL: [Column; Column::COUNT] = [
        Column::Id,
        Column::Time,
        Column::Model,
        Column::Prompt,
        Column::Compl,
        Column::Think,
        Column::Ttft,
        Column::Total,
        Column::Reason,
        Column::Status,
    ];

    /// The column's index in `ALL`.
    pub const fn index(self) -> usize {
        match self {
            Column::Id => 0,
            Column::Time => 1,
            Column::Model => 2,
            Column::Prompt => 3,
            Column::Compl => 4,
            Column::Think => 5,
            Column::Ttft => 6,
            Column::Total => 7,
            Column::Reason => 8,
            Column::Status => 9,
        }
    }
}

/// The number of dividers between the `Column::COUNT` columns.
pub const SLIDER_COUNT: usize = Column::COUNT - 1;

/// The minimum width (px) of each column, in `Column` order (M7): a divider
/// cannot drag either adjacent column below its minimum.
pub const MIN_WIDTHS: [u32; Column::COUNT] = [32, 85, 100, 48, 48, 52, 52, 48, 100, 56];

/// The default column widths, in `Column` order (M7). The fixed columns'
/// defaults are independent of the window width; the `Model` column default
/// is the remainder at the default window width (1280px) — `rebalance`
/// adjusts it to the actual window width at startup and on resize.
pub const DEFAULT_WIDTHS: [u32; Column::COUNT] = [40, 105, 456, 60, 60, 65, 65, 60, 150, 75];

/// The tolerated difference (logical px) between the saved and the current
/// window width for the saved column widths to be restored (M7).
pub const WIDTH_TOLERANCE_PX: f32 = 1.0;

/// The request-table column widths (M7), in `Column` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnWidths {
    pub widths: [u32; Column::COUNT],
}

impl Default for ColumnWidths {
    fn default() -> Self {
        Self {
            widths: DEFAULT_WIDTHS,
        }
    }
}

impl ColumnWidths {
    /// The width (px) of `col`.
    pub fn width(&self, col: Column) -> u32 {
        self.widths[col.index()]
    }

    /// The sum of all column widths (px).
    pub fn total(&self) -> u32 {
        self.widths.iter().sum()
    }

    /// A copy with each width raised to its minimum (for hand-edited
    /// configs that stored a width below the limit).
    pub fn clamped(&self) -> Self {
        Self {
            widths: self
                .widths
                .iter()
                .zip(MIN_WIDTHS.iter())
                .map(|(w, min)| w.max(min))
                .copied()
                .collect::<Vec<_>>()
                .try_into()
                .expect("ColumnWidths has exactly COUNT entries"),
        }
    }

    /// Move the divider between `left` and `right` so the left column
    /// becomes `new_left` (px): the right column takes the difference, so
    /// the total is constant, and both columns stay at or above their
    /// minimums.
    pub fn move_divider(&self, left: Column, right: Column, new_left: f32) -> Self {
        let l = self.widths[left.index()];
        let r = self.widths[right.index()];
        let min_left = MIN_WIDTHS[left.index()];
        let min_right = MIN_WIDTHS[right.index()];
        let max_left = (l + r).saturating_sub(min_right).max(min_left);
        let target = (new_left.round() as i64).clamp(min_left as i64, max_left as i64) as u32;
        let mut widths = self.widths;
        widths[left.index()] = target;
        widths[right.index()] = l + r - target;
        Self { widths }
    }
}

/// The columns on either side of the `slider` (0-based) divider: the
/// divider between `ALL[slider]` and `ALL[slider + 1]`. `None` out of range.
pub fn slider_columns(slider: i32) -> Option<(Column, Column)> {
    let i = usize::try_from(slider).ok()?;
    if i >= SLIDER_COUNT {
        return None;
    }
    Some((Column::ALL[i], Column::ALL[i + 1]))
}

/// The width (logical px) available for the `Column::COUNT` table columns
/// at a main window of `window_width` logical px (M7): the window minus the
/// main-area padding (2×8px), the table padding (2×8px), the header-row
/// padding (2×8px), the expand-symbol cell (16px), the cell gap after it
/// (8px), and the `SLIDER_COUNT` dividers (`SLIDER_COUNT`×8px).
pub fn available_column_width(window_width: f32) -> u32 {
    let available = window_width - 8.0 * 6.0 - 16.0 - 8.0 - SLIDER_COUNT as f32 * 8.0;
    available.max(0.0).round() as u32
}

/// Set the flex `Model` column to the remainder at `window_width` (logical
/// px), so the columns always fill the table exactly (M7). The other columns
/// are untouched; the `Model` column is kept at its minimum when the other
/// columns leave no room.
pub fn rebalance(window_width: f32, widths: &mut ColumnWidths) {
    let model = Column::Model;
    let others: u32 = widths
        .widths
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != model.index())
        .map(|(_, w)| *w)
        .sum();
    let available = available_column_width(window_width);
    widths.widths[model.index()] = available
        .saturating_sub(others)
        .max(MIN_WIDTHS[model.index()]);
}

/// The persisted request-table column widths (M7): the widths and the
/// (logical px) window width they were saved at. The widths are restored on
/// startup only when the window width is unchanged, or the window opens
/// maximized (see `resolve_restore`); a missing or malformed entry yields
/// `None`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct TableColumns {
    #[serde(rename = "window-width")]
    pub window_width: f32,
    pub id: u32,
    pub time: u32,
    pub model: u32,
    pub prompt: u32,
    pub compl: u32,
    pub think: u32,
    pub ttft: u32,
    pub total: u32,
    pub reason: u32,
    pub status: u32,
}

impl TableColumns {
    /// The widths as `ColumnWidths` (not clamped; clamp with
    /// `ColumnWidths::clamped`).
    pub fn widths(&self) -> ColumnWidths {
        ColumnWidths {
            widths: [
                self.id,
                self.time,
                self.model,
                self.prompt,
                self.compl,
                self.think,
                self.ttft,
                self.total,
                self.reason,
                self.status,
            ],
        }
    }

    /// A `TableColumns` for the current widths and window width (M7).
    pub fn from_widths(window_width: f32, widths: &ColumnWidths) -> Self {
        let w = &widths.widths;
        Self {
            window_width,
            id: w[Column::Id.index()],
            time: w[Column::Time.index()],
            model: w[Column::Model.index()],
            prompt: w[Column::Prompt.index()],
            compl: w[Column::Compl.index()],
            think: w[Column::Think.index()],
            ttft: w[Column::Ttft.index()],
            total: w[Column::Total.index()],
            reason: w[Column::Reason.index()],
            status: w[Column::Status.index()],
        }
    }
}

/// Parse the `table_columns` object from a config JSON object, tolerantly
/// (M7). Returns `None` when `window-width` is missing or not a positive
/// finite number, or any of the ten column widths is missing or not a
/// non-negative integer, so a malformed entry can never restore bogus
/// widths.
pub fn parse_table_columns(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Option<TableColumns> {
    let get_u32 = |key: &str| {
        object
            .get(key)
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok())
    };
    let window_width = object
        .get("window-width")
        .and_then(|v| v.as_f64())
        .and_then(|v| {
            let f = v as f32;
            (f.is_finite() && f > 0.0).then_some(f)
        })?;
    Some(TableColumns {
        window_width,
        id: get_u32("id")?,
        time: get_u32("time")?,
        model: get_u32("model")?,
        prompt: get_u32("prompt")?,
        compl: get_u32("compl")?,
        think: get_u32("think")?,
        ttft: get_u32("ttft")?,
        total: get_u32("total")?,
        reason: get_u32("reason")?,
        status: get_u32("status")?,
    })
}

/// The column widths to use at startup (M7): the saved (clamped) widths when
/// the window width is the same as before (within `WIDTH_TOLERANCE_PX`),
/// otherwise the defaults. When `maximized` is true (the window opens
/// maximized) the width check is skipped: the pre-`run` width is the last
/// normal (or default) width, not the maximized width the saved widths may
/// have been recorded at, and the caller rebalances the flex `Model` column
/// to the actual width right after, so the saved widths are safe to restore.
pub fn resolve_restore(
    saved: Option<&TableColumns>,
    window_width: f32,
    maximized: bool,
) -> ColumnWidths {
    match saved {
        Some(t) if maximized => t.widths().clamped(),
        Some(t) if (t.window_width - window_width).abs() <= WIDTH_TOLERANCE_PX => {
            t.widths().clamped()
        }
        _ => ColumnWidths::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_fill_the_default_window() {
        let w = ColumnWidths::default();
        assert_eq!(w.width(Column::Id), 40);
        assert_eq!(w.width(Column::Time), 105);
        assert_eq!(w.width(Column::Model), 456);
        assert_eq!(w.width(Column::Status), 75);
        // The fixed columns sum to 680; at the default 1280px window the
        // Model column is the remainder (1136 - 680 = 456).
        assert_eq!(w.total(), 1136);
        assert_eq!(available_column_width(1280.0), 1136);
    }

    #[test]
    fn available_width_tracks_the_window() {
        assert_eq!(available_column_width(1024.0), 880);
        assert_eq!(available_column_width(1408.0), 1264);
        assert_eq!(available_column_width(100.0), 0);
    }

    #[test]
    fn move_divider_moves_both_columns_and_keeps_the_total() {
        let w = ColumnWidths::default();
        let r = w.move_divider(Column::Id, Column::Time, 60.0);
        assert_eq!(r.width(Column::Id), 60);
        assert_eq!(r.width(Column::Time), 85);
        assert_eq!(r.total(), w.total());
        assert_eq!(
            r.width(Column::Model),
            w.width(Column::Model),
            "the other columns are untouched"
        );
    }

    #[test]
    fn move_divider_clamps_at_the_left_minimum() {
        let w = ColumnWidths::default();
        let r = w.move_divider(Column::Id, Column::Time, -100.0);
        assert_eq!(r.width(Column::Id), MIN_WIDTHS[Column::Id.index()]);
        assert_eq!(
            r.width(Column::Time),
            w.width(Column::Id) + w.width(Column::Time) - MIN_WIDTHS[Column::Id.index()]
        );
        assert_eq!(r.total(), w.total());
    }

    #[test]
    fn move_divider_clamps_at_the_right_minimum() {
        let w = ColumnWidths::default();
        let r = w.move_divider(Column::Id, Column::Time, 10_000.0);
        assert_eq!(
            r.width(Column::Id),
            w.width(Column::Id) + w.width(Column::Time) - MIN_WIDTHS[Column::Time.index()]
        );
        assert_eq!(r.width(Column::Time), MIN_WIDTHS[Column::Time.index()]);
        assert_eq!(r.total(), w.total());
    }

    #[test]
    fn move_divider_rounds_half_values() {
        let w = ColumnWidths::default();
        let r = w.move_divider(Column::Id, Column::Time, 50.5);
        assert_eq!(r.width(Column::Id), 51);
        assert_eq!(r.width(Column::Time), 94);
    }

    #[test]
    fn move_divider_works_with_the_model_column() {
        let w = ColumnWidths::default();
        // The divider between Model and Prompt: the Model column absorbs.
        let r = w.move_divider(Column::Model, Column::Prompt, 356.0);
        assert_eq!(r.width(Column::Model), 356);
        assert_eq!(r.width(Column::Prompt), 60 + (456 - 356));
        assert_eq!(r.total(), w.total());
    }

    #[test]
    fn rebalance_sets_the_model_column_to_the_remainder() {
        let mut w = ColumnWidths::default();
        rebalance(1280.0, &mut w);
        assert_eq!(w.width(Column::Model), 456);
        rebalance(1408.0, &mut w);
        assert_eq!(w.width(Column::Model), 456 + 128);
        assert_eq!(w.width(Column::Id), 40, "the fixed columns are untouched");
        assert_eq!(w.total(), available_column_width(1408.0));
    }

    #[test]
    fn rebalance_keeps_the_model_column_at_its_minimum() {
        let mut w = ColumnWidths::default();
        // A very narrow window leaves no room: the Model column stays at its
        // minimum (the other columns would overflow, which the table clips).
        rebalance(700.0, &mut w);
        assert_eq!(w.width(Column::Model), MIN_WIDTHS[Column::Model.index()]);
    }

    #[test]
    fn clamped_raises_widths_below_the_minimum() {
        let w = ColumnWidths {
            widths: [10, 85, 90, 48, 48, 52, 52, 48, 100, 56],
        };
        let c = w.clamped();
        assert_eq!(c.width(Column::Id), MIN_WIDTHS[Column::Id.index()]);
        assert_eq!(c.width(Column::Model), MIN_WIDTHS[Column::Model.index()]);
        assert_eq!(c.width(Column::Time), 85, "in-range widths are kept");
    }

    #[test]
    fn slider_columns_maps_each_divider_to_its_columns() {
        assert_eq!(slider_columns(0), Some((Column::Id, Column::Time)));
        assert_eq!(slider_columns(1), Some((Column::Time, Column::Model)));
        assert_eq!(slider_columns(8), Some((Column::Reason, Column::Status)));
        assert_eq!(slider_columns(9), None);
        assert_eq!(slider_columns(-1), None);
    }

    fn table_columns() -> TableColumns {
        TableColumns::from_widths(1280.0, &ColumnWidths::default())
    }

    #[test]
    fn resolve_restore_restores_when_the_window_width_matches() {
        let saved = table_columns();
        assert_eq!(
            resolve_restore(Some(&saved), 1280.0, false),
            ColumnWidths::default()
        );
        // The tolerance absorbs sub-pixel drift.
        assert_eq!(
            resolve_restore(Some(&saved), 1280.5, false),
            ColumnWidths::default()
        );
    }

    #[test]
    fn resolve_restore_defaults_when_the_window_width_changed() {
        let mut saved = table_columns();
        saved.id = 64;
        let w = resolve_restore(Some(&saved), 1400.0, false);
        assert_eq!(w, ColumnWidths::default());
        assert_eq!(w.width(Column::Id), 40, "the saved widths are not used");
    }

    #[test]
    fn resolve_restore_defaults_without_saved_widths() {
        assert_eq!(
            resolve_restore(None, 1280.0, false),
            ColumnWidths::default()
        );
        // Even maximized, there is nothing to restore.
        assert_eq!(resolve_restore(None, 1280.0, true), ColumnWidths::default());
    }

    #[test]
    fn resolve_restore_clamps_saved_widths_below_the_minimum() {
        let mut saved = table_columns();
        saved.id = 5;
        let w = resolve_restore(Some(&saved), 1280.0, false);
        assert_eq!(w.width(Column::Id), MIN_WIDTHS[Column::Id.index()]);
    }

    #[test]
    fn resolve_restore_ignores_the_width_mismatch_when_maximized() {
        let mut saved = table_columns();
        saved.id = 64;
        // Saved while maximized (the saved width is the maximized width); the
        // pre-`run` width is the normal one, so without the exception the
        // mismatch would fall back to the defaults.
        let w = resolve_restore(Some(&saved), 1400.0, true);
        assert_eq!(w.width(Column::Id), 64, "the saved widths are restored");
        // Not maximized: the same mismatch keeps the defaults.
        let d = resolve_restore(Some(&saved), 1400.0, false);
        assert_eq!(d, ColumnWidths::default());
    }

    #[test]
    fn parse_table_columns_round_trips() {
        let saved = table_columns();
        let value: serde_json::Value = serde_json::to_value(saved).unwrap();
        let parsed = parse_table_columns(value.as_object().unwrap()).unwrap();
        assert_eq!(parsed, saved);
    }

    #[test]
    fn parse_table_columns_rejects_missing_or_bad_fields() {
        // A missing width field.
        let mut value: serde_json::Value = serde_json::to_value(table_columns()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("id");
        assert!(parse_table_columns(object).is_none());

        // A wrong-typed width field.
        let mut value: serde_json::Value = serde_json::to_value(table_columns()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.insert("id".to_owned(), serde_json::json!("wide"));
        assert!(parse_table_columns(object).is_none());

        // A non-positive window width.
        let mut value: serde_json::Value = serde_json::to_value(table_columns()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.insert("window-width".to_owned(), serde_json::json!(0.0));
        assert!(parse_table_columns(object).is_none());

        // A non-finite window width.
        let mut value: serde_json::Value = serde_json::to_value(table_columns()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.insert("window-width".to_owned(), serde_json::json!(1e308));
        assert!(parse_table_columns(object).is_none());

        // A negative width.
        let mut value: serde_json::Value = serde_json::to_value(table_columns()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.insert("status".to_owned(), serde_json::json!(-5));
        assert!(parse_table_columns(object).is_none());
    }
}
