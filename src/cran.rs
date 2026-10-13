use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

const PACKAGES_URL: &str = "https://cran.r-project.org/src/contrib/PACKAGES";

/// Package -> version for everything currently on CRAN. `$PTRACK_CRAN_PACKAGES`
/// may point to a local copy of the PACKAGES index (offline use, tests).
pub fn cran_index() -> Result<HashMap<String, String>> {
    let text = match std::env::var("PTRACK_CRAN_PACKAGES") {
        Ok(f) => fs::read_to_string(&f).with_context(|| format!("reading {}", f))?,
        Err(_) => {
            let out = Command::new("curl")
                .args(["-sfL", "--max-time", "60", PACKAGES_URL])
                .output()
                .context("running curl")?;
            if !out.status.success() {
                bail!("could not download {} (curl exit {})", PACKAGES_URL, out.status);
            }
            String::from_utf8_lossy(&out.stdout).into_owned()
        }
    };
    let mut idx = HashMap::new();
    for rec in text.split("\n\n") {
        let f = dcf_fields(rec);
        if let (Some(p), Some(v)) = (f.get("Package"), f.get("Version")) {
            idx.insert(p.clone(), v.clone());
        }
    }
    if idx.is_empty() {
        bail!("CRAN index is empty or unreadable");
    }
    Ok(idx)
}

/// Top-level fields of a DCF record (continuation lines are ignored).
pub fn dcf_fields(rec: &str) -> HashMap<String, String> {
    rec.lines()
        .filter(|l| !l.starts_with([' ', '\t']))
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

pub struct LocalPkg {
    pub name: String,
    pub version: String,
}

fn read_pkg(dir: &Path) -> Option<LocalPkg> {
    let f = dcf_fields(&fs::read_to_string(dir.join("DESCRIPTION")).ok()?);
    Some(LocalPkg { name: f.get("Package")?.clone(), version: f.get("Version")?.clone() })
}

/// The R package of a project folder: its own DESCRIPTION, or one in a direct
/// subfolder. With several candidates, the one named like the slug wins.
pub fn find_package(folder: &Path, slug: &str) -> Result<Option<LocalPkg>, Vec<String>> {
    let mut cands: Vec<LocalPkg> = read_pkg(folder).into_iter().collect();
    if let Ok(entries) = fs::read_dir(folder) {
        let mut subs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        subs.sort();
        cands.extend(subs.iter().filter_map(|d| read_pkg(d)));
    }
    if let Some(i) = cands.iter().position(|c| c.name.eq_ignore_ascii_case(slug)) {
        return Ok(Some(cands.swap_remove(i)));
    }
    match cands.len() {
        0 => Ok(None),
        1 => Ok(cands.pop()),
        _ => Err(cands.iter().map(|c| c.name.clone()).collect()),
    }
}

/// R version order: numeric components split on '.' and '-'.
pub fn cmp_version(a: &str, b: &str) -> Ordering {
    let parts = |s: &str| -> Vec<u64> {
        s.split(['.', '-']).map(|x| x.trim().parse().unwrap_or(0)).collect()
    };
    let (pa, pb) = (parts(a), parts(b));
    for i in 0..pa.len().max(pb.len()) {
        let o = pa.get(i).unwrap_or(&0).cmp(pb.get(i).unwrap_or(&0));
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(cmp_version("0.10.0", "0.9.2"), Ordering::Greater);
        assert_eq!(cmp_version("1.2", "1.2.0"), Ordering::Equal);
        assert_eq!(cmp_version("1.1-2", "1.1.3"), Ordering::Less);
        assert_eq!(cmp_version("0.9.2.9000", "0.9.2"), Ordering::Greater);
    }

    #[test]
    fn dcf() {
        let f = dcf_fields("Package: demopkg\nVersion: 1.1.2\nImports: Rcpp,\n    stats\n");
        assert_eq!(f["Package"], "demopkg");
        assert_eq!(f["Version"], "1.1.2");
        assert_eq!(f["Imports"], "Rcpp,");
    }
}
