// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

//! The draggable split-panel fractions (FR-7.5).
//!
//! The dashboard's middle area is divided into three vertically stacked
//! sections — the charts, the request table, and the server-info panel — by
//! two draggable splitters. The division is described by two fractions:
//!
//! - `charts`: the share of the section space given to the charts.
//! - `table`: the share of the *remaining* space (after the charts) given to
//!   the request table; the server-info panel takes whatever is left.
//!
//! The default gives the charts half the space and splits the other half
//! equally between the request table and the server-info panel. The fractions
//! are persisted in the config and restored on startup.

use serde::Serialize;

/// Default share of the section space for the charts (half).
pub const DEFAULT_CHARTS_FRAC: f32 = 0.5;
/// Default share of the remaining space for the request table (half, so the
/// server-info panel gets the other half).
pub const DEFAULT_TABLE_FRAC: f32 = 0.5;

/// The charts may take at most this share of the section space, leaving at
/// least `1 - MAX_CHARTS_FRAC` for the table and server-info combined.
pub const MAX_CHARTS_FRAC: f32 = 0.8;
/// The charts may take at least this share of the section space.
pub const MIN_CHARTS_FRAC: f32 = 0.1;
/// The request table may take at most this share of the remaining space.
pub const MAX_TABLE_FRAC: f32 = 0.9;
/// The request table may take at least this share of the remaining space.
pub const MIN_TABLE_FRAC: f32 = 0.1;

/// The persisted split-panel fractions (FR-7.5).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SplitPanels {
    /// Share of the section space given to the charts.
    pub charts: f32,
    /// Share of the remaining space given to the request table.
    pub table: f32,
}

impl Default for SplitPanels {
    fn default() -> Self {
        Self {
            charts: DEFAULT_CHARTS_FRAC,
            table: DEFAULT_TABLE_FRAC,
        }
    }
}

impl SplitPanels {
    /// Both fractions clamped to the supported ranges.
    pub fn clamped(&self) -> Self {
        Self {
            charts: self.charts.clamp(MIN_CHARTS_FRAC, MAX_CHARTS_FRAC),
            table: self.table.clamp(MIN_TABLE_FRAC, MAX_TABLE_FRAC),
        }
    }

    /// A copy with the charts fraction replaced (and clamped).
    pub fn with_charts(&self, charts: f32) -> Self {
        Self {
            charts: charts.clamp(MIN_CHARTS_FRAC, MAX_CHARTS_FRAC),
            table: self.table,
        }
    }

    /// A copy with the table fraction replaced (and clamped).
    pub fn with_table(&self, table: f32) -> Self {
        Self {
            charts: self.charts,
            table: table.clamp(MIN_TABLE_FRAC, MAX_TABLE_FRAC),
        }
    }
}

/// Parse the `split_panels` object from a config JSON object, tolerantly.
///
/// A missing or wrong-typed field keeps its default without discarding the
/// other. The returned fractions are not clamped; the caller clamps them (see
/// `SplitPanels::clamped`).
pub fn parse_split_panels(object: &serde_json::Map<String, serde_json::Value>) -> SplitPanels {
    let get_f32 = |key: &str| object.get(key).and_then(|v| v.as_f64()).map(|v| v as f32);
    let charts = get_f32("charts").unwrap_or(DEFAULT_CHARTS_FRAC);
    let table = get_f32("table").unwrap_or(DEFAULT_TABLE_FRAC);
    SplitPanels { charts, table }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_give_charts_half_and_split_the_rest() {
        let s = SplitPanels::default();
        assert_eq!(s.charts, DEFAULT_CHARTS_FRAC);
        assert_eq!(s.table, DEFAULT_TABLE_FRAC);
        assert_eq!(s.charts, 0.5);
        assert_eq!(s.table, 0.5);
    }

    #[test]
    fn clamped_keeps_in_range_values() {
        let s = SplitPanels {
            charts: 0.5,
            table: 0.5,
        };
        assert_eq!(s.clamped(), s);
    }

    #[test]
    fn clamped_bounds_charts_fraction() {
        assert_eq!(
            SplitPanels {
                charts: 0.0,
                table: 0.5
            }
            .clamped()
            .charts,
            MIN_CHARTS_FRAC
        );
        assert_eq!(
            SplitPanels {
                charts: 1.0,
                table: 0.5
            }
            .clamped()
            .charts,
            MAX_CHARTS_FRAC
        );
        assert_eq!(
            SplitPanels {
                charts: -3.0,
                table: 0.5
            }
            .clamped()
            .charts,
            MIN_CHARTS_FRAC
        );
    }

    #[test]
    fn clamped_bounds_table_fraction() {
        assert_eq!(
            SplitPanels {
                charts: 0.5,
                table: 0.0
            }
            .clamped()
            .table,
            MIN_TABLE_FRAC
        );
        assert_eq!(
            SplitPanels {
                charts: 0.5,
                table: 1.0
            }
            .clamped()
            .table,
            MAX_TABLE_FRAC
        );
    }

    #[test]
    fn with_charts_replaces_and_clamps_only_charts() {
        let s = SplitPanels {
            charts: 0.5,
            table: 0.3,
        };
        assert_eq!(
            s.with_charts(0.7),
            SplitPanels {
                charts: 0.7,
                table: 0.3
            }
        );
        assert_eq!(
            s.with_charts(0.99),
            SplitPanels {
                charts: MAX_CHARTS_FRAC,
                table: 0.3
            }
        );
        assert_eq!(
            s.with_charts(0.01),
            SplitPanels {
                charts: MIN_CHARTS_FRAC,
                table: 0.3
            }
        );
    }

    #[test]
    fn with_table_replaces_and_clamps_only_table() {
        let s = SplitPanels {
            charts: 0.6,
            table: 0.5,
        };
        assert_eq!(
            s.with_table(0.4),
            SplitPanels {
                charts: 0.6,
                table: 0.4
            }
        );
        assert_eq!(
            s.with_table(0.99),
            SplitPanels {
                charts: 0.6,
                table: MAX_TABLE_FRAC
            }
        );
        assert_eq!(
            s.with_table(0.01),
            SplitPanels {
                charts: 0.6,
                table: MIN_TABLE_FRAC
            }
        );
    }

    #[test]
    fn parse_reads_both_fields() {
        let value: serde_json::Value = serde_json::json!({ "charts": 0.7, "table": 0.3 });
        let object = value.as_object().unwrap();
        assert_eq!(
            parse_split_panels(object),
            SplitPanels {
                charts: 0.7,
                table: 0.3
            }
        );
    }

    #[test]
    fn parse_defaults_missing_fields_independently() {
        let value: serde_json::Value = serde_json::json!({ "charts": 0.7 });
        let object = value.as_object().unwrap();
        assert_eq!(
            parse_split_panels(object),
            SplitPanels {
                charts: 0.7,
                table: DEFAULT_TABLE_FRAC
            }
        );

        let value: serde_json::Value = serde_json::json!({ "table": 0.3 });
        let object = value.as_object().unwrap();
        assert_eq!(
            parse_split_panels(object),
            SplitPanels {
                charts: DEFAULT_CHARTS_FRAC,
                table: 0.3
            }
        );
    }

    #[test]
    fn parse_defaults_wrong_typed_fields() {
        let value: serde_json::Value = serde_json::json!({ "charts": "wide", "table": [1, 2] });
        let object = value.as_object().unwrap();
        assert_eq!(parse_split_panels(object), SplitPanels::default());
    }

    #[test]
    fn parse_keeps_out_of_range_values_for_the_caller_to_clamp() {
        let value: serde_json::Value = serde_json::json!({ "charts": 0.95, "table": 0.05 });
        let object = value.as_object().unwrap();
        let parsed = parse_split_panels(object);
        assert_eq!(parsed.charts, 0.95);
        assert_eq!(parsed.table, 0.05);
        assert_eq!(
            parsed.clamped(),
            SplitPanels {
                charts: MAX_CHARTS_FRAC,
                table: MIN_TABLE_FRAC
            }
        );
    }
}
