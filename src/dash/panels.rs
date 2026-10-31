//! Panel types (R-B2): what each one needs from its query, and the safe text panels. Markdown is
//! rendered here and HTML is sanitised here, so a stored or imported dashboard can never carry
//! script to the viewer; the page shows both inside a sandboxed frame as a second layer.

use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Time first, then values: `N N+` (one series per column) or `N T N` (time, series, value).
    Series,
    /// Label then values: `T N+`.
    Category,
    /// Bars take either of the two above.
    Either,
    /// `x y value`, the value numeric.
    Heatmap,
    /// One number, from the first row.
    Single,
    /// Latitude then longitude, then optional value and label.
    Geo,
    /// `source target value`.
    Sankey,
    Any,
}

#[derive(Debug, Clone, Copy)]
pub struct Info {
    pub name: &'static str,
    pub shape: Shape,
    /// Markdown and HTML panels have content instead of a query.
    pub static_content: bool,
}

const fn p(name: &'static str, shape: Shape) -> Info {
    Info {
        name,
        shape,
        static_content: false,
    }
}

pub const TYPES: &[Info] = &[
    p("line", Shape::Series),
    p("area", Shape::Series),
    p("stacked_area", Shape::Series),
    p("scatter", Shape::Series),
    p("bar_v", Shape::Either),
    p("bar_h", Shape::Either),
    p("stacked_bar_v", Shape::Either),
    p("stacked_bar_h", Shape::Either),
    p("pie", Shape::Category),
    p("donut", Shape::Category),
    p("heatmap", Shape::Heatmap),
    p("stat", Shape::Single),
    p("gauge", Shape::Single),
    p("table", Shape::Any),
    p("geo_map", Shape::Geo),
    p("sankey", Shape::Sankey),
    Info {
        name: "markdown",
        shape: Shape::Any,
        static_content: true,
    },
    Info {
        name: "html",
        shape: Shape::Any,
        static_content: true,
    },
];

pub fn info(name: &str) -> Option<Info> {
    TYPES.iter().copied().find(|t| t.name == name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Col {
    /// Numbers (and timestamps, which arrive as milliseconds).
    N,
    /// Text.
    T,
}

/// Column kinds of a result, from its rows: numeric when every non-null value is a number.
pub fn kinds(rows: &[Value]) -> (Vec<String>, Vec<Col>) {
    let Some(first) = rows.first().and_then(Value::as_object) else {
        return (vec![], vec![]);
    };
    let names: Vec<String> = first.keys().cloned().collect();
    let kinds = names
        .iter()
        .map(|k| {
            let vals: Vec<&Value> = rows
                .iter()
                .map(|r| &r[k.as_str()])
                .filter(|v| !v.is_null())
                .collect();
            // A column that is all null counts as numeric: nothing contradicts it.
            if vals.iter().all(|v| v.is_number()) {
                Col::N
            } else {
                Col::T
            }
        })
        .collect();
    (names, kinds)
}

/// Does a result of these column kinds fit the panel type? The error says what it needs.
pub fn check_shape(ty: &str, cols: &[Col]) -> Result<(), String> {
    use Col::{N, T};
    let Some(info) = info(ty) else {
        return Err(format!("unknown panel type {ty:?}"));
    };
    let series =
        cols.len() >= 2 && cols[0] == N && cols[1..].iter().all(|c| *c == N) || cols == [N, T, N];
    let category = cols.len() >= 2 && cols[0] == T && cols[1..].iter().all(|c| *c == N);
    let ok = match info.shape {
        Shape::Series => series,
        Shape::Category => category,
        Shape::Either => series || category,
        Shape::Heatmap => cols.len() == 3 && cols[2] == N,
        Shape::Single => cols.contains(&N),
        Shape::Geo => cols.len() >= 2 && cols[0] == N && cols[1] == N,
        Shape::Sankey => cols == [T, T, N],
        Shape::Any => true,
    };
    if ok {
        return Ok(());
    }
    Err(match info.shape {
        Shape::Series => "a time series panel needs a time column then numeric columns, or time, series name, value",
        Shape::Category => "this panel needs a text label column then numeric columns",
        Shape::Either => "a bar panel needs a text label then numbers, or a time column then numbers",
        Shape::Heatmap => "a heatmap needs three columns: x, y and a numeric value",
        Shape::Single => "this panel needs at least one numeric column",
        Shape::Geo => "a map needs latitude and longitude as the first two numeric columns",
        Shape::Sankey => "a sankey needs three columns: source (text), target (text), value (number)",
        Shape::Any => "",
    }
    .to_string())
}

/// Whether PromQL series become `[time, series, value]` rows (true) or the last value of each
/// series, `[series, value]` (false), for this panel type.
pub fn wants_time(ty: &str) -> bool {
    matches!(
        ty,
        "line" | "area" | "stacked_area" | "scatter" | "heatmap" | "table"
    )
}

/// A series' name: its label value when it has one label, else `k="v", ...`.
fn series_label(labels: &Value) -> String {
    let Some(m) = labels.as_object().filter(|m| !m.is_empty()) else {
        return "value".to_string();
    };
    if m.len() == 1 {
        return m
            .values()
            .next()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
    }
    m.iter()
        .map(|(k, v)| format!("{k}=\"{}\"", v.as_str().unwrap_or_default()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// PromQL series as the table a panel expects.
pub fn promql_rows(ty: &str, series: &[Value]) -> Vec<Value> {
    let mut rows = vec![];
    if wants_time(ty) {
        for s in series {
            let name = series_label(&s["labels"]);
            for p in s["points"].as_array().into_iter().flatten() {
                rows.push(json!({"time": p[0], "series": name, "value": p[1]}));
            }
        }
    } else {
        for s in series {
            if let Some(last) = s["points"].as_array().and_then(|p| p.last()) {
                rows.push(json!({"series": series_label(&s["labels"]), "value": last[1]}));
            }
        }
    }
    rows
}

/// Pivot long rows into a grid: one row per distinct `row` value, one column per distinct `column`
/// value, cells the sum of `value`, plus a `Total` column and a `Total` row. Order of first
/// appearance is kept. A row that lacks one of the three columns is skipped.
pub fn pivot(rows: &[Value], row: &str, col: &str, val: &str) -> (Vec<String>, Vec<Value>) {
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        o => o.to_string(),
    };
    let (mut rkeys, mut ckeys): (Vec<String>, Vec<String>) = (vec![], vec![]);
    let mut cells: std::collections::HashMap<(String, String), f64> = Default::default();
    for r in rows {
        let (Some(a), Some(b), Some(v)) =
            (r.get(row), r.get(col), r.get(val).and_then(Value::as_f64))
        else {
            continue;
        };
        let (a, b) = (text(a), text(b));
        if !rkeys.contains(&a) {
            rkeys.push(a.clone());
        }
        if !ckeys.contains(&b) {
            ckeys.push(b.clone());
        }
        *cells.entry((a, b)).or_default() += v;
    }
    let mut columns = vec![row.to_string()];
    columns.extend(ckeys.iter().cloned());
    columns.push("Total".into());
    let mut out = vec![];
    let mut col_totals = vec![0f64; ckeys.len()];
    for a in &rkeys {
        let mut m = serde_json::Map::new();
        m.insert(row.to_string(), json!(a));
        let mut total = 0.0;
        for (i, b) in ckeys.iter().enumerate() {
            let v = cells.get(&(a.clone(), b.clone())).copied().unwrap_or(0.0);
            col_totals[i] += v;
            total += v;
            m.insert(b.clone(), json!(v));
        }
        m.insert("Total".into(), json!(total));
        out.push(Value::Object(m));
    }
    let mut m = serde_json::Map::new();
    m.insert(row.to_string(), json!("Total"));
    for (b, t) in ckeys.iter().zip(&col_totals) {
        m.insert(b.clone(), json!(t));
    }
    m.insert("Total".into(), json!(col_totals.iter().sum::<f64>()));
    out.push(Value::Object(m));
    (columns, out)
}

// ---- safe text ------------------------------------------------------------------------

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn safe_url(u: &str) -> bool {
    let l = u.trim().to_ascii_lowercase();
    (l.starts_with("https://") || l.starts_with("http://") || l.starts_with("mailto:"))
        && !l.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Bold, italic, code and links over already escaped text.
fn inline(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let find = |from: usize, pat: &str| -> Option<usize> {
        let p: Vec<char> = pat.chars().collect();
        (from..chars.len().saturating_sub(p.len() - 1)).find(|&j| chars[j..j + p.len()] == p[..])
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '`'
            && let Some(e) = find(i + 1, "`")
        {
            out.push_str(&format!(
                "<code>{}</code>",
                chars[i + 1..e].iter().collect::<String>()
            ));
            i = e + 1;
        } else if c == '*'
            && chars.get(i + 1) == Some(&'*')
            && let Some(e) = find(i + 2, "**")
        {
            out.push_str(&format!(
                "<strong>{}</strong>",
                inline(&chars[i + 2..e].iter().collect::<String>())
            ));
            i = e + 2;
        } else if c == '*'
            && let Some(e) = find(i + 1, "*")
            && e > i + 1
        {
            out.push_str(&format!(
                "<em>{}</em>",
                inline(&chars[i + 1..e].iter().collect::<String>())
            ));
            i = e + 1;
        } else if c == '['
            && let Some(close) = find(i + 1, "](")
            && let Some(end) = find(close + 2, ")")
        {
            let label: String = chars[i + 1..close].iter().collect();
            let url: String = chars[close + 2..end].iter().collect();
            // The text was escaped, so a URL holds no quote that could end the attribute.
            if safe_url(&url.replace("&amp;", "&")) {
                out.push_str(&format!("<a href=\"{url}\" rel=\"noopener noreferrer nofollow\" target=\"_blank\">{label}</a>"));
                i = end + 1;
            } else {
                out.push(c);
                i += 1;
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// Markdown to HTML: headings, paragraphs, bullet and numbered lists, fenced code, bold, italic,
/// inline code and http(s)/mailto links. Everything else is text. Nothing from the input reaches
/// the output unescaped, and the result is sanitised again.
pub fn markdown_to_html(md: &str) -> String {
    let mut out = String::new();
    let mut para: Vec<String> = vec![];
    let mut list: Option<&'static str> = None;
    let mut code: Option<Vec<String>> = None;
    let flush = |out: &mut String, para: &mut Vec<String>| {
        if !para.is_empty() {
            out.push_str(&format!("<p>{}</p>", para.join(" ")));
            para.clear();
        }
    };
    let close = |out: &mut String, list: &mut Option<&'static str>| {
        if let Some(t) = list.take() {
            out.push_str(&format!("</{t}>"));
        }
    };
    for raw in md.lines() {
        if let Some(buf) = code.as_mut() {
            if raw.trim_start().starts_with("```") {
                out.push_str(&format!("<pre><code>{}</code></pre>", buf.join("\n")));
                code = None;
            } else {
                buf.push(esc(raw));
            }
            continue;
        }
        let line = raw.trim_end();
        let t = line.trim_start();
        if t.starts_with("```") {
            flush(&mut out, &mut para);
            close(&mut out, &mut list);
            code = Some(vec![]);
        } else if t.is_empty() {
            flush(&mut out, &mut para);
            close(&mut out, &mut list);
        } else if let Some(level) = (1..=6)
            .rev()
            .find(|n| t.starts_with(&format!("{} ", "#".repeat(*n))))
        {
            flush(&mut out, &mut para);
            close(&mut out, &mut list);
            out.push_str(&format!(
                "<h{level}>{}</h{level}>",
                inline(&esc(t[level + 1..].trim()))
            ));
        } else if let Some(item) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            flush(&mut out, &mut para);
            if list != Some("ul") {
                close(&mut out, &mut list);
                out.push_str("<ul>");
                list = Some("ul");
            }
            out.push_str(&format!("<li>{}</li>", inline(&esc(item))));
        } else if let Some((n, item)) = t
            .split_once(". ")
            .filter(|(n, _)| !n.is_empty() && n.len() < 4 && n.bytes().all(|b| b.is_ascii_digit()))
        {
            let _ = n;
            flush(&mut out, &mut para);
            if list != Some("ol") {
                close(&mut out, &mut list);
                out.push_str("<ol>");
                list = Some("ol");
            }
            out.push_str(&format!("<li>{}</li>", inline(&esc(item))));
        } else {
            close(&mut out, &mut list);
            para.push(inline(&esc(t)));
        }
    }
    if let Some(buf) = code {
        out.push_str(&format!("<pre><code>{}</code></pre>", buf.join("\n")));
    }
    flush(&mut out, &mut para);
    close(&mut out, &mut list);
    sanitize_html(&out)
}

const ALLOWED: &[&str] = &[
    "p",
    "br",
    "hr",
    "b",
    "i",
    "em",
    "strong",
    "u",
    "s",
    "code",
    "pre",
    "blockquote",
    "ul",
    "ol",
    "li",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "a",
    "span",
    "div",
    "table",
    "thead",
    "tbody",
    "tr",
    "th",
    "td",
    "small",
    "sub",
    "sup",
];
const VOID: &[&str] = &["br", "hr"];
/// Elements dropped together with everything inside them.
const DROP_WITH_CONTENT: &[&str] = &[
    "script", "style", "iframe", "object", "embed", "template", "noscript", "svg", "math", "title",
    "textarea", "select", "option", "form", "frame", "frameset", "applet", "base", "head",
];
const MAX_DEPTH: usize = 64;

/// A parsed tag: name lower-cased, whether it closes, attributes, index after the `>`.
type Tag = (String, bool, Vec<(String, String)>, usize);

/// Parse one tag starting at `<`.
fn parse_tag(c: &[char], start: usize) -> Option<Tag> {
    let mut i = start + 1;
    let close = c.get(i) == Some(&'/');
    if close {
        i += 1;
    }
    let ns = i;
    while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '-' || c[i] == ':') {
        i += 1;
    }
    if i == ns || !c[ns].is_ascii_alphabetic() {
        return None;
    }
    let name: String = c[ns..i].iter().collect::<String>().to_ascii_lowercase();
    let mut attrs = vec![];
    loop {
        while i < c.len() && (c[i].is_whitespace() || c[i] == '/') {
            i += 1;
        }
        match c.get(i) {
            None => return None,
            Some('>') => return Some((name, close, attrs, i + 1)),
            _ => {}
        }
        let ks = i;
        while i < c.len() && !c[i].is_whitespace() && !matches!(c[i], '=' | '>' | '/') {
            i += 1;
        }
        let key: String = c[ks..i].iter().collect::<String>().to_ascii_lowercase();
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        let mut val = String::new();
        if c.get(i) == Some(&'=') {
            i += 1;
            while i < c.len() && c[i].is_whitespace() {
                i += 1;
            }
            match c.get(i) {
                Some(&q) if q == '"' || q == '\'' => {
                    i += 1;
                    let vs = i;
                    while i < c.len() && c[i] != q {
                        i += 1;
                    }
                    if i >= c.len() {
                        return None;
                    }
                    val = c[vs..i].iter().collect();
                    i += 1;
                }
                _ => {
                    let vs = i;
                    while i < c.len() && !c[i].is_whitespace() && c[i] != '>' {
                        i += 1;
                    }
                    val = c[vs..i].iter().collect();
                }
            }
        }
        if !key.is_empty() {
            attrs.push((key, val));
        }
    }
}

/// Keep only an allowlist of tags and attributes; drop scripts and the like with their content;
/// escape every `<` and `>` that is not part of a kept tag. Links must be http(s) or mailto.
pub fn sanitize_html(html: &str) -> String {
    let c: Vec<char> = html.chars().collect();
    let (mut out, mut i) = (String::new(), 0);
    let mut open: Vec<String> = vec![];
    while i < c.len() {
        if c[i] != '<' {
            match c[i] {
                '>' => out.push_str("&gt;"),
                ch => out.push(ch),
            }
            i += 1;
            continue;
        }
        if c[i..].starts_with(&['<', '!', '-', '-']) {
            // A comment: dropped.
            let rest: String = c[i + 4..].iter().collect();
            i = rest
                .find("-->")
                .map_or(c.len(), |p| i + 4 + rest[..p].chars().count() + 3);
            continue;
        }
        let Some((name, close, attrs, next)) = parse_tag(&c, i) else {
            // Not a tag we can read: its `<` is text.
            if matches!(c.get(i + 1), Some('!' | '?')) {
                i = c[i..]
                    .iter()
                    .position(|ch| *ch == '>')
                    .map_or(c.len(), |p| i + p + 1);
            } else {
                out.push_str("&lt;");
                i += 1;
            }
            continue;
        };
        i = next;
        if DROP_WITH_CONTENT.contains(&name.as_str()) {
            if !close {
                let rest: String = c[i..].iter().collect::<String>().to_ascii_lowercase();
                let end = format!("</{name}");
                i = rest.find(&end).map_or(c.len(), |p| {
                    let after = i + rest[..p].chars().count();
                    c[after..]
                        .iter()
                        .position(|ch| *ch == '>')
                        .map_or(c.len(), |q| after + q + 1)
                });
            }
            continue;
        }
        if !ALLOWED.contains(&name.as_str()) {
            continue; // the tag goes, its text stays
        }
        if close {
            if let Some(pos) = open.iter().rposition(|n| *n == name) {
                while open.len() > pos {
                    if let Some(n) = open.pop() {
                        out.push_str(&format!("</{n}>"));
                    }
                }
            }
            continue;
        }
        if open.len() >= MAX_DEPTH {
            continue;
        }
        let mut tag = format!("<{name}");
        for (k, v) in &attrs {
            match (name.as_str(), k.as_str()) {
                (_, "title") => tag.push_str(&format!(" title=\"{}\"", esc(v))),
                ("a", "href") if safe_url(v) => {
                    tag.push_str(&format!(" href=\"{}\"", esc(v.trim())))
                }
                ("td" | "th", "colspan" | "rowspan")
                    if v.len() <= 2 && v.bytes().all(|b| b.is_ascii_digit()) && !v.is_empty() =>
                {
                    tag.push_str(&format!(" {k}=\"{v}\""))
                }
                _ => {}
            }
        }
        if name == "a" {
            tag.push_str(" rel=\"noopener noreferrer nofollow\" target=\"_blank\"");
        }
        tag.push('>');
        out.push_str(&tag);
        if !VOID.contains(&name.as_str()) {
            open.push(name);
        }
    }
    while let Some(n) = open.pop() {
        out.push_str(&format!("</{n}>"));
    }
    out
}
