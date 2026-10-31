//! Message templates (R-A4): `{{name}}` is replaced from a fixed set of variables. Unknown names
//! render as empty text and are reported, so a typo is visible in the test-send. The output is
//! plain text; destinations decide how to carry it (JSON string, mail body), so a value can never
//! change the structure of what is sent.

use std::collections::BTreeMap;

pub const DEFAULT_SUBJECT: &str = "[grepui] {{state}}: {{alert.name}}";
pub const DEFAULT_BODY: &str = "{{alert.name}} is {{state}}\nvalue: {{value}} (threshold {{threshold}})\nlabels: {{labels}}\nat: {{time}}\norg: {{org}}";

/// The variable names a template may use. `labels.<key>` is also allowed.
pub const VARIABLES: &[&str] = &[
    "alert.name",
    "alert.id",
    "state",
    "value",
    "threshold",
    "labels",
    "time",
    "org",
    "outcome",
];

/// Render `tpl`; returns the text and the names that were not known.
pub fn render(tpl: &str, vars: &BTreeMap<String, String>) -> (String, Vec<String>) {
    let (mut out, mut unknown) = (String::new(), vec![]);
    let mut rest = tpl;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(j) = after.find("}}") else {
            out.push_str(&rest[i..]);
            rest = "";
            break;
        };
        let name = after[..j].trim();
        match vars.get(name) {
            Some(v) => out.push_str(v),
            None => {
                if !unknown.iter().any(|u| u == name) {
                    unknown.push(name.to_string());
                }
            }
        }
        rest = &after[j + 2..];
    }
    out.push_str(rest);
    (clean(&out), unknown)
}

/// Control characters other than newline and tab are dropped, so a label value cannot smuggle
/// terminal or header injection into a message.
fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(10_000)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_known_and_reports_unknown() {
        let v = BTreeMap::from([("state".to_string(), "firing".to_string())]);
        let (s, u) = render("is {{ state }} {{nope}} {{ open", &v);
        assert_eq!(s, "is firing  {{ open");
        assert_eq!(u, vec!["nope"]);
    }
}
