//! Just enough CSV (RFC 4180): quoted fields, `""` escapes, CRLF or LF, an optional BOM. Used to
//! read enrichment uploads and to redact CSV exports.

/// Parse `text` into rows. Fails on an unterminated quote or when a limit is exceeded.
pub fn parse(text: &str, max_rows: usize, max_cols: usize) -> Result<Vec<Vec<String>>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let (mut rows, mut row, mut field) = (vec![], vec![], String::new());
    let (mut quoted, mut any) = (false, false);
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if quoted {
            match c {
                '"' if it.peek() == Some(&'"') => {
                    field.push('"');
                    it.next();
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => {
                quoted = true;
                any = true;
            }
            ',' => {
                row.push(std::mem::take(&mut field));
                any = true;
            }
            '\r' => {}
            '\n' => {
                if any || !field.is_empty() {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                any = false;
            }
            _ => {
                field.push(c);
                any = true;
            }
        }
        if row.len() > max_cols {
            return Err(format!("more than {max_cols} columns"));
        }
        if rows.len() > max_rows {
            return Err(format!("more than {max_rows} rows"));
        }
    }
    if quoted {
        return Err("a quoted field is not closed".into());
    }
    if any || !field.is_empty() {
        row.push(field);
        rows.push(row);
    }
    if rows.iter().any(|r| r.len() > max_cols) {
        return Err(format!("more than {max_cols} columns"));
    }
    if rows.len() > max_rows {
        return Err(format!("more than {max_rows} rows"));
    }
    Ok(rows)
}

/// One field as CSV text.
pub fn field(f: &str) -> String {
    if f.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", f.replace('"', "\"\""))
    } else {
        f.to_string()
    }
}

pub fn write(rows: &[Vec<String>]) -> String {
    rows.iter()
        .map(|r| r.iter().map(|f| field(f)).collect::<Vec<_>>().join(","))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_and_newlines() {
        let r = parse("a,b\n\"x, \"\"y\"\"\",\"l1\nl2\"\r\n\n3,\n", 10, 5).unwrap();
        assert_eq!(
            r,
            vec![vec!["a", "b"], vec!["x, \"y\"", "l1\nl2"], vec!["3", ""]]
        );
        assert_eq!(parse(&write(&r), 10, 5).unwrap(), r);
        assert!(parse("\"open", 10, 5).is_err());
        assert!(parse("a,b,c", 10, 2).is_err());
    }
}
