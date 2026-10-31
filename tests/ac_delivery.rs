//! AC0.1.12 – AC0.1.13: version single source, banned name, pinned workflows.
use std::{fs, path::Path};

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if matches!(
            name.as_str(),
            "target" | ".git" | "node_modules" | ".serena"
        ) {
            continue;
        }
        if p.is_dir() {
            walk(&p, f);
        } else {
            f(&p);
        }
    }
}

#[test]
fn ac_0_1_12_version_matches_cargo() {
    let v = fs::read_to_string("VERSION").unwrap();
    assert_eq!(
        v.trim(),
        env!("CARGO_PKG_VERSION"),
        "VERSION and Cargo.toml must agree (make version-check)"
    );
    assert!(
        grepui::VERSION.ends_with(env!("CARGO_PKG_VERSION")),
        "{}",
        grepui::VERSION
    );
}

#[test]
fn ac_0_1_12_no_banned_name() {
    // Built at run time so this file does not contain the word it forbids.
    let banned = ["open", "observe"].concat();
    let mut hits = vec![];
    walk(Path::new("."), &mut |p| {
        if let Ok(text) = fs::read_to_string(p)
            && text.to_lowercase().contains(&banned)
        {
            hits.push(p.display().to_string());
        }
    });
    assert!(hits.is_empty(), "banned name found in: {hits:?}");
}

fn workflows() -> Vec<(String, String)> {
    let mut out = vec![];
    for e in fs::read_dir(".github/workflows").unwrap().flatten() {
        out.push((
            e.file_name().to_string_lossy().into_owned(),
            fs::read_to_string(e.path()).unwrap(),
        ));
    }
    out
}

#[test]
fn ac_0_1_13_actions_pinned() {
    let wf = workflows();
    assert!(wf.len() >= 3, "ci, release and docker workflows");
    for (name, text) in wf {
        for line in text
            .lines()
            .filter(|l| l.trim_start().trim_start_matches("- ").starts_with("uses:"))
        {
            let r = line.split("uses:").nth(1).unwrap().trim();
            let spec = r.split_whitespace().next().unwrap();
            let sha = spec.split('@').nth(1).unwrap_or_default();
            assert!(
                sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
                "{name}: not pinned to a commit SHA: {line}"
            );
        }
    }
}

#[test]
fn ac_0_1_13_release_checks_tag() {
    let get = |n: &str| {
        workflows()
            .into_iter()
            .find(|(f, _)| f == n)
            .unwrap_or_else(|| panic!("{n} missing"))
            .1
    };
    let gate = r#"test "$TAG" = "v$(cat VERSION)""#;
    assert!(get("release.yml").contains(gate));
    assert!(get("docker.yml").contains(gate));
    let docker = get("docker.yml");
    assert!(
        docker.contains("ubuntu-24.04-arm")
            && docker.contains("linux/amd64")
            && docker.contains("linux/arm64")
    );
    assert!(
        !docker.contains("setup-qemu"),
        "native builds only, no emulation"
    );
}

#[test]
fn ac_0_1_13_security_workflow_scans_on_schedule() {
    let sec = workflows()
        .into_iter()
        .find(|(f, _)| f == "security.yml")
        .expect("security.yml")
        .1;
    for needle in [
        "semgrep",
        "p/rust",
        "p/secrets",
        "p/github-actions",
        "trivy-action",
        "scan-type: fs",
        "image-ref:",
        "cron:",
        "upload-sarif",
        "severity: CRITICAL,HIGH",
    ] {
        assert!(sec.contains(needle), "security.yml should contain {needle}");
    }
    // Least privilege: read-only by default; only SARIF-uploading jobs get security-events: write.
    let top = sec.split("\njobs:").next().unwrap();
    assert!(
        top.contains("contents: read") && !top.contains("security-events"),
        "write permissions belong on jobs, not the workflow"
    );
    assert_eq!(
        sec.matches("security-events: write").count(),
        3,
        "one per scanning job"
    );
    // The runtime image must stay free of OpenSSL (grepui does no TLS).
    let docker = fs::read_to_string("Dockerfile").unwrap();
    assert!(
        docker.contains("base-nossl"),
        "runtime image without libssl"
    );
}
