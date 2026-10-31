//! AC0.18.1 to AC0.18.3: the guides and the README match the product. A page, flag, variable,
//! command or route that exists must be written down, a screenshot that is named must exist, and
//! a link must lead somewhere.
use std::{collections::BTreeSet, fs, path::Path};

use grepui::api::ROUTES;

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

/// Image file names named by `image::x.png[` (AsciiDoc) or `](images/x.png)` (Markdown).
fn images(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for l in text.lines() {
        if let Some(r) = l.trim_start().strip_prefix("image::")
            && let Some(i) = r.find('[')
        {
            out.insert(r[..i].to_string());
        }
        let mut rest = l;
        while let Some(i) = rest.find("](images/") {
            rest = &rest[i + 9..];
            if let Some(j) = rest.find(')') {
                out.insert(rest[..j].to_string());
            }
        }
    }
    out
}

/// The `(path)` targets of Markdown links that are files in this repository.
fn local_links(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(i) = rest.find("](") {
        rest = &rest[i + 2..];
        let Some(j) = rest.find(')') else { break };
        let t = &rest[..j];
        if !t.starts_with("http") && !t.starts_with('#') && !t.starts_with("mailto:") {
            out.insert(t.split('#').next().unwrap_or(t).to_string());
        }
    }
    out
}

/// Pages in the left column: the second string of every `['key', 'Label']` in shell.js.
fn nav_labels() -> Vec<String> {
    let shell = read("web/static/js/components/shell.js");
    let start = shell.find("const LIVE = [").expect("LIVE list");
    let body = &shell[start..shell[start..].find("];").map(|e| start + e).expect("end")];
    body.lines()
        .filter_map(|l| {
            let l = l.trim();
            let (_, rest) = l.strip_prefix("['")?.split_once("', '")?;
            Some(rest.split('\'').next()?.to_string())
        })
        .collect()
}

/// The `Config` fields with their environment variables.
fn config_flags() -> Vec<(String, String)> {
    let cfg = read("src/config.rs");
    let end = cfg.find("impl Default for Config").expect("impl");
    let body = &cfg[..end];
    let mut out = vec![];
    let mut env = String::new();
    let mut long = None;
    for l in body.lines() {
        let l = l.trim();
        if let Some(i) = l.find("env = \"") {
            env = l[i + 7..].split('"').next().unwrap_or("").to_string();
        }
        if let Some(i) = l.find("long = \"") {
            long = Some(l[i + 8..].split('"').next().unwrap_or("").to_string());
        }
        if let Some(f) = l.strip_prefix("pub ")
            && let Some((name, _)) = f.split_once(':')
        {
            let flag = long.take().unwrap_or_else(|| name.replace('_', "-"));
            out.push((format!("--{flag}"), std::mem::take(&mut env)));
        }
    }
    out
}

/// A route with every `{name}` parameter reduced to `{}`.
fn shape(route: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for c in route.chars() {
        match c {
            '{' => {
                skip = true;
                out.push_str("{}");
            }
            '}' => skip = false,
            _ if !skip => out.push(c),
            _ => {}
        }
    }
    out
}

// ---- AC0.18.1 the User Guide ----

#[test]
fn ac_0_18_1_user_guide_images_exist() {
    let imgs = images(&read("docs/guide/user-guide.adoc"));
    assert!(
        imgs.len() >= 40,
        "the guide shows the product: {}",
        imgs.len()
    );
    for i in imgs {
        assert!(
            Path::new("images").join(&i).is_file(),
            "images/{i} is named but missing"
        );
    }
}

#[test]
fn ac_0_18_1_user_guide_covers_every_page() {
    let guide = read("docs/guide/user-guide.adoc");
    let labels = nav_labels();
    assert!(labels.len() >= 19, "found {labels:?}");
    for l in &labels {
        assert!(
            guide.contains(l.as_str()),
            "the page {l:?} is in the menu but not in the User Guide"
        );
    }
    for l in ["Administration", "Reports", "Platform"] {
        assert!(guide.contains(l), "{l} is missing");
    }
}

#[test]
fn ac_0_18_1_user_guide_states_the_roles_and_sign_in() {
    let g = read("docs/guide/user-guide.adoc");
    for w in [
        "viewer",
        "editor",
        "admin",
        "first administrator",
        "Show SQL",
        "Time range",
        "31 days",
    ] {
        assert!(g.contains(w), "the User Guide should explain {w:?}");
    }
}

// ---- AC0.18.2 the Technical Guide ----

#[test]
fn ac_0_18_2_every_flag_and_variable_documented() {
    let g = read("docs/guide/technical-guide.adoc");
    let flags = config_flags();
    assert!(flags.len() >= 30, "parsed {} flags", flags.len());
    for (flag, env) in flags {
        assert!(
            g.contains(&flag),
            "{flag} is a flag but not in the Technical Guide"
        );
        if !env.is_empty() {
            assert!(
                g.contains(&env),
                "{env} is an environment variable but not in the Technical Guide"
            );
        }
    }
}

#[test]
fn ac_0_18_2_commands_and_defaults_documented() {
    let g = read("docs/guide/technical-guide.adoc");
    for c in [
        "grepui serve",
        "grepui migrate",
        "grepui playground",
        "grepui healthcheck",
        "--demo-data",
        "--llm-data",
        "--no-live",
        "--print",
        "--wait",
    ] {
        assert!(g.contains(c), "{c} is missing from the Technical Guide");
    }
    // The defaults in the guide are the ones in the code.
    for d in [
        "127.0.0.1:8080",
        "mysql://root@127.0.0.1:4002/public",
        "opentelemetry_traces",
        "opentelemetry_logs",
        "grepui-demo-admin",
        "1.3.0",
    ] {
        assert!(g.contains(d), "default {d} is missing");
    }
}

#[test]
fn ac_0_18_2_documented_routes_exist() {
    let g = read("docs/guide/technical-guide.adoc");
    let start = g.find("=== A few routes").expect("routes table");
    let table = &g[start
        ..g[start..]
            .find("=== The API is frozen")
            .map(|e| start + e)
            .expect("end")];
    let mut seen = 0;
    for tok in table.split('`').skip(1).step_by(2) {
        if !tok.starts_with('/') {
            continue;
        }
        let path = tok.split('?').next().unwrap_or(tok);
        let full = if path.starts_with("/healthz") || path.starts_with("/api/") {
            path.to_string()
        } else {
            format!("/api/v1/orgs/{{org}}{path}")
        };
        assert!(
            ROUTES.iter().any(|r| shape(r) == shape(&full)),
            "the Technical Guide names {tok} but no such route is registered ({full})"
        );
        seen += 1;
    }
    assert!(seen >= 15, "only {seen} routes found in the table");
}

// ---- AC0.18.3 the README ----

#[test]
fn ac_0_18_3_readme_screenshots_exist() {
    let imgs = images(&read("README.md"));
    assert!(
        imgs.len() >= 10,
        "the README should show the product: {}",
        imgs.len()
    );
    for i in imgs {
        assert!(
            Path::new("images").join(&i).is_file(),
            "images/{i} is named but missing"
        );
    }
}

#[test]
fn ac_0_18_3_readme_links_resolve() {
    let text = read("README.md");
    let links = local_links(&text);
    assert!(links.len() >= 8);
    for l in links {
        assert!(
            Path::new(&l).exists(),
            "README links to {l}, which does not exist"
        );
    }
    for l in local_links(&read("docs/guide/README.md")) {
        assert!(
            Path::new("docs/guide").join(&l).exists(),
            "docs/guide/README.md links to {l}"
        );
    }
    for g in [
        "docs/guide/user-guide.adoc",
        "docs/guide/technical-guide.adoc",
    ] {
        assert!(text.contains(g), "the README must point to {g}");
    }
}

#[test]
fn ac_0_18_3_readme_has_the_quick_start() {
    let t = read("README.md");
    for w in [
        "make db-up",
        "make playground",
        "grepui-demo-admin",
        "grepui migrate",
        "v1.3.0",
    ] {
        assert!(t.contains(w), "the README should show {w:?}");
    }
}
