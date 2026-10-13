use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ptrack-test-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn ptrack(root: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ptrack"))
        .args(args)
        .current_dir(cwd)
        .env("PTRACK_REGISTRY", root.join(".registry"))
        .output()
        .unwrap()
}

fn ok(root: &Path, args: &[&str]) -> String {
    let o = ptrack(root, root, args);
    assert!(o.status.success(), "{:?} failed: {}", args, String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

fn fails(root: &Path, args: &[&str]) -> String {
    let o = ptrack(root, root, args);
    assert!(!o.status.success(), "{:?} should fail", args);
    String::from_utf8(o.stderr).unwrap()
}

#[test]
fn lifecycle() {
    let root = scratch("lifecycle");
    fs::create_dir_all(root.join("PAPERS/aurora")).unwrap();
    ok(&root, &["init", root.to_str().unwrap()]);

    assert!(ok(&root, &["new", "paper", "Aurora", "--path", "PAPERS/aurora", "--priority", "high"])
        .starts_with("p0001  aurora"));
    assert!(ok(&root, &["new", "software", "quickknn", "--status", "published"]).starts_with("p0002"));

    // Uniqueness: slug, folder, and id-shaped slugs are refused without burning an id.
    assert!(fails(&root, &["new", "paper", "x", "--slug", "aurora"]).contains("already used"));
    assert!(fails(&root, &["new", "paper", "x", "--path", "PAPERS/aurora"]).contains("already the folder"));
    assert!(fails(&root, &["new", "paper", "x", "--slug", "p0009"]).contains("looks like an id"));
    assert!(ok(&root, &["new", "paper", "third"]).starts_with("p0003"));

    // Submission trail and status transitions.
    ok(&root, &["submit", "aurora", "Journal of Examples", "--date", "2026-06-01"]);
    ok(&root, &["outcome", "aurora", "rejected", "--date", "2026-10-10"]);
    ok(&root, &["submit", "aurora", "Annals of Fiction"]);
    let toml = fs::read_to_string(root.join(".registry/p0001.toml")).unwrap();
    assert!(toml.contains("outcome = \"rejected\"") && toml.contains("status = \"submitted\""));

    // Closed projects are hidden by default.
    let ls = ok(&root, &["ls"]);
    assert!(ls.contains("aurora") && !ls.contains("quickknn"));
    assert!(ok(&root, &["ls", "--all"]).contains("quickknn"));

    // Project resolved from the .ptrack-id file in the working directory.
    let o = ptrack(&root, &root.join("PAPERS/aurora"), &["show"]);
    assert!(String::from_utf8(o.stdout).unwrap().contains("Annals of Fiction"));

    ok(&root, &["set", "third", "next=draft intro", "tags=a,b"]);
    assert!(fails(&root, &["set", "third", "id=p0099"]).contains("cannot be changed"));
    ok(&root, &["deadline", "third", "2000-01-01", "old"]);
    assert!(ok(&root, &["due"]).contains("OVERDUE"));
    ok(&root, &["deadline", "third", "--rm", "1"]);
    assert_eq!(ok(&root, &["due"]).trim(), "(nothing)");

    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn links_scan_check_export() {
    let root = scratch("step2");
    for d in ["SOFTWARE/project-a/a-package", "PAPERS/paper-a", "PAPERS/old"] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    fs::write(root.join("SOFTWARE/project-a/a-package/DESCRIPTION"), "Package: a\nVersion: 1.2.0\n").unwrap();
    ok(&root, &["init", root.to_str().unwrap()]);
    ok(&root, &["new", "software", "a", "--path", "SOFTWARE/project-a", "--tag", "cran"]);
    ok(&root, &["new", "paper", "A paper", "--slug", "paper-a", "--path", "PAPERS/old"]);

    // Links store ids, show both directions, and are removable by number.
    ok(&root, &["link", "paper-a", "uses", "a"]);
    assert!(fails(&root, &["link", "paper-a", "uses", "a"]).contains("already"));
    assert!(fails(&root, &["link", "a", "uses", "a"]).contains("itself"));
    assert!(ok(&root, &["show", "paper-a"]).contains("1. uses p0001 a"));
    assert!(ok(&root, &["show", "a"]).contains("p0002 paper-a uses"));
    ok(&root, &["link", "paper-a", "--rm", "1"]);
    assert!(!ok(&root, &["show", "paper-a"]).contains("links"));

    // Scan: a moved folder is detected and fixed; a lost id file is rewritten.
    fs::remove_dir_all(root.join("PAPERS/paper-a")).unwrap();
    fs::rename(root.join("PAPERS/old"), root.join("PAPERS/paper-a")).unwrap();
    fs::remove_file(root.join("SOFTWARE/project-a/.ptrack-id")).unwrap();
    let s = ok(&root, &["scan"]);
    assert!(s.contains("moved") && s.contains("no id file"), "{s}");
    ok(&root, &["scan", "--fix"]);
    assert_eq!(ok(&root, &["scan"]).trim(), "(nothing)");
    assert!(ok(&root, &["path", "paper-a"]).trim().ends_with("PAPERS/paper-a"));

    // Check against a local CRAN index.
    let idx = root.join("PACKAGES");
    fs::write(&idx, "Package: a\nVersion: 1.10.0\n\nPackage: b\nVersion: 0.1\n").unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_ptrack"))
        .arg("check")
        .current_dir(&root)
        .env("PTRACK_REGISTRY", root.join(".registry"))
        .env("PTRACK_CRAN_PACKAGES", &idx)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&o.stdout).contains("BEHIND CRAN"));

    // Export refuses to clobber hand-written files, then regenerates its own.
    ok(&root, &["deadline", "a", "2000-01-01", "resubmit"]);
    let md = ok(&root, &["export"]);
    assert!(md.contains("## software (1)") && md.contains("overdue"));
    fs::write(root.join("TODO.md"), "my notes\n").unwrap();
    assert!(fails(&root, &["export", "--out", "TODO.md"]).contains("refusing"));
    ok(&root, &["export", "--out", "GEN.md"]);
    ok(&root, &["export", "--out", "GEN.md"]);

    fs::remove_dir_all(&root).unwrap();
}
