//! Scheduled reports (R-E1, ADR 0014): the pure parts. A report names a dashboard or a saved logs
//! view, a format (HTML or CSV), a cron schedule and recipients. Rendering turns result rows into
//! text; the API module runs the queries and sends the mail.

use crate::alerts::{cron::Cron, valid_email};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_RECIPIENTS: usize = 20;
/// Rows of one section shown in an HTML report (a CSV report keeps every row it ran).
pub const HTML_ROWS: usize = 200;

fn day() -> u32 {
    24
}
fn on() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub name: String,
    /// `dashboard` or `view` (a saved logs view).
    pub kind: String,
    /// The dashboard or view id.
    pub target: String,
    /// `html` or `csv`.
    pub format: String,
    /// Five-field cron, UTC.
    pub schedule: String,
    /// How far back from the run the data reaches.
    #[serde(default = "day")]
    pub range_hours: u32,
    pub recipients: Vec<String>,
    #[serde(default = "on")]
    pub enabled: bool,
}

impl Report {
    pub fn validate(&self) -> Result<(), String> {
        let n = self.name.trim();
        if n.is_empty() || n.len() > 120 || n.chars().any(char::is_control) {
            return Err("the report needs a name of up to 120 characters".into());
        }
        if !matches!(self.kind.as_str(), "dashboard" | "view") {
            return Err("kind must be dashboard or view".into());
        }
        if self.target.is_empty() || self.target.len() > 64 {
            return Err("name the dashboard or view to report on".into());
        }
        if !matches!(self.format.as_str(), "html" | "csv") {
            return Err("format must be html or csv (PDF is not supported)".into());
        }
        Cron::parse(&self.schedule).map_err(|e| format!("schedule: {e}"))?;
        if self.range_hours == 0 || self.range_hours > 24 * 31 {
            return Err("range_hours must be between 1 and 744".into());
        }
        if self.recipients.is_empty() || self.recipients.len() > MAX_RECIPIENTS {
            return Err(format!("name between 1 and {MAX_RECIPIENTS} recipients"));
        }
        if let Some(a) = self.recipients.iter().find(|a| !valid_email(a)) {
            return Err(format!("{a:?} is not an email address"));
        }
        Ok(())
    }
}

/// What a run knows after it ran: kept per report so the list can show it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReportState {
    /// The schedule slot (epoch ms, on the minute) that was last taken.
    #[serde(default)]
    pub last_slot_ms: i64,
    #[serde(default)]
    pub last_run_ms: i64,
    /// `ok`, `failed` or empty before the first run.
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub runs: u64,
}

/// The latest schedule slot after `last_slot_ms` that is not in the future, if any. A long gap
/// gives one run (the latest slot), not one per missed slot.
pub fn due_slot(cron: &Cron, last_slot_ms: i64, now_ms: i64) -> Option<i64> {
    let mut slot = None;
    let mut from = last_slot_ms;
    for _ in 0..4000 {
        match cron.next_after(from) {
            Some(t) if t <= now_ms => {
                slot = Some(t);
                from = t;
            }
            _ => break,
        }
    }
    slot
}

/// One table of a report: a dashboard panel, or the rows of a saved view.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub title: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub note: Option<String>,
}

/// A value as text. Numbers keep their digits; a null is empty.
pub fn cell(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

/// A self-contained page: inline styles only, nothing to load, every value escaped.
pub fn render_html(title: &str, subtitle: &str, sections: &[Section]) -> String {
    let mut o = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{t}</title></head>\
         <body style=\"font-family:-apple-system,Segoe UI,Helvetica,Arial,sans-serif;color:#1f2933;max-width:960px;margin:24px auto;padding:0 16px\">\
         <h1 style=\"font-size:22px;margin:0 0 4px\">{t}</h1><p style=\"color:#52606d;margin:0 0 20px\">{s}</p>",
        t = esc(title),
        s = esc(subtitle)
    );
    if sections.is_empty() {
        o.push_str("<p>Nothing to show: the report has no data panels.</p>");
    }
    for s in sections {
        o.push_str(&format!(
            "<h2 style=\"font-size:16px;margin:24px 0 8px\">{}</h2>",
            esc(&s.title)
        ));
        if s.rows.is_empty() {
            o.push_str("<p style=\"color:#52606d\">No rows in this period.</p>");
        } else {
            o.push_str(
                "<table style=\"border-collapse:collapse;font-size:13px;width:100%\"><thead><tr>",
            );
            for c in &s.columns {
                o.push_str(&format!(
                    "<th style=\"text-align:left;border-bottom:2px solid #cbd2d9;padding:4px 8px\">{}</th>",
                    esc(c)
                ));
            }
            o.push_str("</tr></thead><tbody>");
            for r in s.rows.iter().take(HTML_ROWS) {
                o.push_str("<tr>");
                for v in r {
                    o.push_str(&format!(
                        "<td style=\"border-bottom:1px solid #e4e7eb;padding:4px 8px\">{}</td>",
                        esc(v)
                    ));
                }
                o.push_str("</tr>");
            }
            o.push_str("</tbody></table>");
            if s.rows.len() > HTML_ROWS {
                o.push_str(&format!(
                    "<p style=\"color:#52606d\">First {HTML_ROWS} of {} rows; a CSV report carries them all.</p>",
                    s.rows.len()
                ));
            }
        }
        if let Some(n) = &s.note {
            o.push_str(&format!("<p style=\"color:#b44d12\">{}</p>", esc(n)));
        }
    }
    o.push_str("</body></html>");
    o
}

/// CSV text: one block per section, headed by `# title` when there is more than one.
pub fn render_csv(sections: &[Section]) -> String {
    let mut out = String::new();
    for (i, s) in sections.iter().enumerate() {
        if sections.len() > 1 {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&format!("# {}\n", s.title.replace(['\n', '\r'], " ")));
        }
        let mut rows = vec![s.columns.clone()];
        rows.extend(s.rows.iter().cloned());
        out.push_str(&crate::csv::write(&rows));
        out.push('\n');
    }
    out
}

/// Objects as rows of text under `columns` (a missing key is empty).
pub fn table(columns: &[String], rows: &[Value]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|r| columns.iter().map(|c| cell(&r[c.as_str()])).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rep() -> Report {
        Report {
            name: "Daily".into(),
            kind: "dashboard".into(),
            target: "abc".into(),
            format: "html".into(),
            schedule: "0 8 * * *".into(),
            range_hours: 24,
            recipients: vec!["ops@example.com".into()],
            enabled: true,
        }
    }

    #[test]
    fn validation_names_the_problem() {
        assert!(rep().validate().is_ok());
        type Edit = fn(&mut Report);
        let cases: [(Edit, &str); 6] = [
            (|r: &mut Report| r.format = "pdf".into(), "html or csv"),
            (|r: &mut Report| r.schedule = "nonsense".into(), "schedule"),
            (|r: &mut Report| r.recipients = vec![], "recipients"),
            (
                |r: &mut Report| r.recipients = vec!["no-at".into()],
                "email",
            ),
            (|r: &mut Report| r.kind = "alert".into(), "kind"),
            (|r: &mut Report| r.range_hours = 0, "range_hours"),
        ];
        for (f, want) in cases {
            let mut r = rep();
            f(&mut r);
            let e = r.validate().unwrap_err();
            assert!(e.contains(want), "{e}");
        }
    }

    #[test]
    fn html_escapes_every_value() {
        let s = Section {
            title: "<b>t</b>".into(),
            columns: vec!["a<".into()],
            rows: vec![vec!["<script>alert(1)</script>".into()]],
            note: None,
        };
        let h = render_html("T&C", "x", &[s]);
        assert!(!h.contains("<script>") && !h.contains("<b>t"));
        assert!(h.contains("&lt;script&gt;") && h.contains("T&amp;C"));
    }

    #[test]
    fn csv_has_header_and_quotes() {
        let s = Section {
            title: "t".into(),
            columns: vec!["a".into(), "b".into()],
            rows: vec![vec!["x,y".into(), "z".into()]],
            note: None,
        };
        let c = render_csv(&[s]);
        assert!(c.starts_with("a,b\n") || c.starts_with("a,b\r\n"), "{c:?}");
        assert!(c.contains("\"x,y\""));
        assert_eq!(
            table(&["a".into()], &[json!({"a": 1})]),
            vec![vec!["1".to_string()]]
        );
    }

    #[test]
    fn a_long_gap_gives_one_run() {
        let c = Cron::parse("*/5 * * * *").unwrap();
        let t0 = 1_793_400_000_000;
        assert_eq!(due_slot(&c, t0, t0 + 1000), None);
        let s = due_slot(&c, t0, t0 + 3 * 3_600_000).unwrap();
        assert!(s > t0 + 3 * 3_600_000 - 300_000 && s <= t0 + 3 * 3_600_000);
    }
}
