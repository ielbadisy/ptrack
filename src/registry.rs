use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::Project;

pub const REGISTRY_DIR: &str = ".registry";
pub const CONFIG_FILE: &str = "ptrack.toml";
pub const ID_FILE: &str = ".ptrack-id";
const DEFAULT_ROOT: &str = "projects";

#[derive(Debug, Serialize, Deserialize)]
struct Config {
    /// Next numeric id to hand out. Only ever increases, so ids are never reused.
    next_id: u32,
}

pub struct Registry {
    pub dir: PathBuf,
}

impl Registry {
    /// Resolution order: $PTRACK_REGISTRY, a `.registry/` found walking up from
    /// the current directory, then ~/projects/.registry.
    pub fn locate() -> Result<Registry> {
        if let Ok(dir) = env::var("PTRACK_REGISTRY") {
            return Registry::open(PathBuf::from(dir));
        }
        let cwd = env::current_dir()?;
        for anc in cwd.ancestors() {
            let cand = anc.join(REGISTRY_DIR);
            if cand.join(CONFIG_FILE).is_file() {
                return Registry::open(cand);
            }
        }
        let home = env::var("HOME").context("HOME not set")?;
        Registry::open(Path::new(&home).join(DEFAULT_ROOT).join(REGISTRY_DIR))
    }

    fn open(dir: PathBuf) -> Result<Registry> {
        if !dir.join(CONFIG_FILE).is_file() {
            bail!(
                "no registry at {} (run `ptrack init <projects-root>`)",
                dir.display()
            );
        }
        Ok(Registry { dir })
    }

    pub fn init(root: &Path) -> Result<Registry> {
        let dir = root.join(REGISTRY_DIR);
        fs::create_dir_all(&dir)?;
        let cfg = dir.join(CONFIG_FILE);
        if cfg.exists() {
            bail!("registry already exists at {}", dir.display());
        }
        write_atomic(&cfg, &toml::to_string(&Config { next_id: 1 })?)?;
        Ok(Registry { dir })
    }

    /// Projects root: the directory holding `.registry/`. Paths are stored relative to it.
    pub fn root(&self) -> PathBuf {
        self.dir.parent().map(Path::to_path_buf).unwrap_or_default()
    }

    pub fn load_all(&self) -> Result<Vec<Project>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let path = entry?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == CONFIG_FILE || path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let text = fs::read_to_string(&path)?;
            let p: Project = toml::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            if format!("{}.toml", p.id) != name {
                bail!("{}: id '{}' does not match file name", path.display(), p.id);
            }
            out.push(p);
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    pub fn save(&self, p: &Project) -> Result<()> {
        let path = self.dir.join(format!("{}.toml", p.id));
        write_atomic(&path, &toml::to_string(p)?)
    }

    pub fn allocate_id(&self) -> Result<String> {
        let cfg_path = self.dir.join(CONFIG_FILE);
        let mut cfg: Config = toml::from_str(&fs::read_to_string(&cfg_path)?)?;
        // Guard against a hand-edited counter colliding with existing files.
        let max_existing = self
            .load_all()?
            .iter()
            .filter_map(|p| p.id.strip_prefix('p').and_then(|n| n.parse::<u32>().ok()))
            .max()
            .unwrap_or(0);
        let n = cfg.next_id.max(max_existing + 1);
        cfg.next_id = n + 1;
        write_atomic(&cfg_path, &toml::to_string(&cfg)?)?;
        Ok(format!("p{:04}", n))
    }

    /// Finds a project by id or slug; with no key, uses the `.ptrack-id` file
    /// found walking up from the current directory.
    pub fn resolve<'a>(&self, all: &'a mut [Project], key: Option<&str>) -> Result<&'a mut Project> {
        let key = match key {
            Some(k) => k.trim().to_string(),
            None => id_from_cwd()?,
        };
        all.iter_mut()
            .find(|p| p.id == key || p.slug == key)
            .ok_or_else(|| anyhow!("no project with id or slug '{}'", key))
    }

    /// Stores `abs` relative to the projects root when it lives under it.
    pub fn rel_path(&self, abs: &Path) -> String {
        let root = self.root();
        match abs.strip_prefix(&root) {
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => abs.to_string_lossy().into_owned(),
        }
    }

    pub fn abs_path(&self, stored: &str) -> PathBuf {
        let p = Path::new(stored);
        if p.is_absolute() { p.to_path_buf() } else { self.root().join(p) }
    }
}

fn id_from_cwd() -> Result<String> {
    let cwd = env::current_dir()?;
    for anc in cwd.ancestors() {
        let f = anc.join(ID_FILE);
        if f.is_file() {
            return Ok(fs::read_to_string(f)?.trim().to_string());
        }
    }
    bail!("no project given and no {} file found above the current directory", ID_FILE)
}

pub fn write_id_file(dir: &Path, id: &str) -> Result<()> {
    write_atomic(&dir.join(ID_FILE), &format!("{}\n", id))
}

fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path)?;
    Ok(())
}
