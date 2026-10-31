//! Dashboard time controls (R-B4) and playlists (R-B6): refresh intervals with a floor, the
//! comparison window, and a rotation that depends only on the clock, so every wall display shows
//! the same dashboard at the same moment.

use crate::{
    error::ApiError,
    query::{MAX_WINDOW_MS, Window},
};
use serde_json::Value;

/// The ranges the page offers (the same keys as the app's time selector).
pub const RANGES: &[&str] = &["15m", "1h", "6h", "24h", "7d"];
pub const REFRESH: &[&str] = &["off", "5s", "10s", "30s", "1m", "5m", "15m", "1h"];
/// No dashboard refreshes faster than this, however it is configured.
pub const REFRESH_FLOOR_MS: i64 = 5_000;
pub const COMPARE: &[&str] = &["1h", "1d", "7d", "prev"];
pub const PLAYLIST_MIN_S: i64 = 10;
pub const PLAYLIST_DEFAULT_S: i64 = 30;

/// Milliseconds between refreshes; `Some(0)` is "off"; `None` for anything not offered.
pub fn refresh_ms(s: &str) -> Option<i64> {
    let ms = match s {
        "off" => 0,
        "5s" => 5_000,
        "10s" => 10_000,
        "30s" => 30_000,
        "1m" => 60_000,
        "5m" => 300_000,
        "15m" => 900_000,
        "1h" => 3_600_000,
        _ => return None,
    };
    (ms == 0 || ms >= REFRESH_FLOOR_MS).then_some(ms)
}

/// How far back the comparison period starts: a fixed amount, or one window length (`prev`).
pub fn compare_offset(w: Window, compare: &str) -> Result<i64, ApiError> {
    Ok(match compare {
        "1h" => 3_600_000,
        "1d" => 86_400_000,
        "7d" => 7 * 86_400_000,
        "prev" => w.to_ms - w.from_ms,
        _ => {
            return Err(ApiError::bad(format!(
                "compare must be one of {}",
                COMPARE.join(", ")
            )));
        }
    })
}

/// The same window, `compare` earlier. Never before the epoch.
pub fn compare_window(w: Window, compare: &str) -> Result<(Window, i64), ApiError> {
    let off = compare_offset(w, compare)?;
    if w.from_ms - off < 0 || w.to_ms - w.from_ms > MAX_WINDOW_MS {
        return Err(ApiError::bad(
            "the comparison period starts before the epoch",
        ));
    }
    Ok((
        Window {
            from_ms: w.from_ms - off,
            to_ms: w.to_ms - off,
            ..w
        },
        off,
    ))
}

/// Move the time column of `rows` forward by `offset_ms`, so the past period lines up under the
/// current one. Rows without a numeric value in `col` are left alone.
pub fn shift_rows(rows: &mut [Value], col: &str, offset_ms: i64) {
    for r in rows {
        if let Some(t) = r.get(col).and_then(Value::as_i64) {
            r[col] = Value::from(t + offset_ms);
        }
    }
}

/// `(index, milliseconds until the next switch)` for `len` dashboards shown `interval_s` each.
pub fn rotation(len: usize, interval_s: i64, now_ms: i64) -> (usize, i64) {
    let slot_ms = interval_s.max(PLAYLIST_MIN_S) * 1000;
    let idx = if len == 0 {
        0
    } else {
        (now_ms.div_euclid(slot_ms) as usize) % len
    };
    (idx, slot_ms - now_ms.rem_euclid(slot_ms))
}
