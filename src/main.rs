mod cran;
mod export;
mod model;
mod registry;
mod scan;
mod tui;

use std::cmp::Ordering;
use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Result};
use chrono::{Local, NaiveDate};
use clap::{Parser, Subcommand};
use comfy_table::{presets, ContentArrangement, Table};

use model::{
    parse_date, parse_span_days, slugify, Deadline, Kind, Link, Outcome, Priority, Project, Status,
    Submission,
};
use registry::{write_id_file, Registry, ID_FILE};

/// Track every project (papers, software, books, courses, ...) under one stable id.
///
/// A project can be named by its id (p0042) or slug (aurora). Where a project
/// argument is optional, or given as ".", the `.ptrack-id` file found walking up
/// from the current directory is used.
#[derive(Parser)]
#[command(name = "ptrack", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Interactive dashboard: filter, sort, edit (default when no command is given)
    Browse,
    /// Create a registry in <ROOT>/.registry (default: current directory)
    Init { root: Option<PathBuf> },
    /// Register a new project and print its id
    New {
        /// paper, software, book, course, grant, consulting, website, dataset, collab, talk, other
        kind: Kind,
        title: String,
        #[arg(long)]
        slug: Option<String>,
        #[arg(long, default_value = "planned")]
        status: Status,
        #[arg(long)]
        priority: Option<Priority>,
        /// Portfolio or programme code, e.g. AMRPRED
        #[arg(long)]
        code: Option<String>,
        /// Target venue: journal, CRAN, conference, publisher
        #[arg(long)]
        venue: Option<String>,
        #[arg(long = "next")]
        next_action: Option<String>,
        /// Project folder; a .ptrack-id file is written into it
        #[arg(long, conflicts_with = "here")]
        path: Option<PathBuf>,
        /// Use the current directory as the project folder
        #[arg(long)]
        here: bool,
        #[arg(long)]
        repo: Option<String>,
        /// Repeatable
        #[arg(long = "collab")]
        collaborators: Vec<String>,
        /// Repeatable
        #[arg(long = "tag")]
        tags: Vec<String>,
    },
    /// List projects (published and archived hidden unless --all)
    Ls {
        #[arg(long)]
        kind: Option<Kind>,
        #[arg(long)]
        status: Option<Status>,
        #[arg(long)]
        priority: Option<Priority>,
        /// Case-insensitive substring of the target venue
        #[arg(long)]
        venue: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        /// Portfolio code (case-insensitive)
        #[arg(long)]
        code: Option<String>,
        #[arg(long, short)]
        all: bool,
    },
    /// Show everything recorded for one project
    Show { project: Option<String> },
    /// Set fields: title slug kind status priority code venue next path repo notes collaborators tags
    ///
    /// An empty value clears an optional field (venue=). Lists take commas (tags=ml,survival).
    Set {
        project: String,
        #[arg(required = true, value_name = "FIELD=VALUE")]
        assignments: Vec<String>,
    },
    /// Record a submission to a venue; sets status=submitted and venue
    Submit {
        project: String,
        venue: String,
        /// Default: today
        #[arg(long)]
        date: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
    /// Record the decision on the latest submission and update status
    Outcome {
        project: String,
        /// under-review, revision, rejected, accepted, published, withdrawn
        outcome: Outcome,
        /// Default: today
        #[arg(long)]
        date: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
    /// Add a deadline, or remove one by its number in `show`
    Deadline {
        project: String,
        /// YYYY-MM-DD
        date: Option<String>,
        what: Vec<String>,
        #[arg(long, conflicts_with_all = ["date", "what"])]
        rm: Option<usize>,
    },
    /// Next action of every open project, high priority first
    Next {
        #[arg(long)]
        kind: Option<Kind>,
    },
    /// Deadlines of open projects: overdue, then upcoming
    Due {
        /// e.g. 30d, 2w, 3m
        #[arg(long, default_value = "30d")]
        within: String,
    },
    /// Print the absolute folder of a project, e.g. cd "$(ptrack path aurora)"
    Path { project: String },
    /// Link two projects (e.g. splits-into, uses, depends-on), or remove link N
    Link {
        project: String,
        rel: Option<String>,
        /// Target project id or slug
        target: Option<String>,
        #[arg(long, conflicts_with_all = ["rel", "target"])]
        rm: Option<usize>,
    },
    /// Compare registry folders with the .ptrack-id files on disk
    ///
    /// Reports moved, missing and untracked folders. --fix records moved folders
    /// and writes missing .ptrack-id files; nothing is deleted.
    Scan {
        #[arg(long)]
        fix: bool,
        #[arg(long, default_value_t = 5)]
        depth: usize,
    },
    /// Compare local R package versions with CRAN
    Check {
        /// Only report packages that need attention
        #[arg(long, short)]
        quiet: bool,
    },
    /// Write a markdown (or --json) view of the registry (stdout unless --out)
    Export {
        #[arg(long)]
        kind: Option<Kind>,
        #[arg(long)]
        tag: Option<String>,
        /// Include published and archived projects
        #[arg(long, short)]
        all: bool,
        /// Deadline horizon, e.g. 60d
        #[arg(long, default_value = "60d")]
        within: String,
        /// Only overwrites a file previously generated by ptrack
        #[arg(long)]
        out: Option<PathBuf>,
        /// Print the selected projects as JSON (for dashboards) instead of markdown
        #[arg(long, conflicts_with = "out")]
        json: bool,
    },
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("ptrack: {:#}", e);
        std::process::exit(1);
    }
}

fn today() -> NaiveDate {
    Local::now().date_naive()
}

fn key(arg: &str) -> Option<&str> {
    if arg == "." { None } else { Some(arg) }
}

fn opt_date(s: Option<String>) -> Result<NaiveDate> {
    s.as_deref().map(parse_date).unwrap_or_else(|| Ok(today()))
}

fn run(cli: Cli) -> Result<()> {
    let cmd = cli.cmd.unwrap_or(Cmd::Browse);
    if let Cmd::Init { root } = &cmd {
        let root = match root {
            Some(r) => r.canonicalize().map_err(|_| anyhow!("folder {} does not exist", r.display()))?,
            None => env::current_dir()?,
        };
        let reg = Registry::init(&root)?;
        println!("registry created at {}", reg.dir.display());
        return Ok(());
    }

    let reg = Registry::locate()?;
    let mut all = reg.load_all()?;

    match cmd {
        Cmd::Browse => tui::run(&reg, all)?,
        Cmd::Init { .. } => unreachable!(),

        Cmd::New {
            kind, title, slug, status, priority, code, venue, next_action, path, here, repo,
            collaborators, tags,
        } => {
            let slug = slug.unwrap_or_else(|| slugify(&title));
            check_slug(&all, &slug, None)?;
            let dir = if here { Some(env::current_dir()?) } else { path };
            let dir = match dir {
                Some(d) => {
                    let d = d.canonicalize().map_err(|_| anyhow!("folder {} does not exist", d.display()))?;
                    if let Some(other) = all.iter().find(|p| p.path.as_deref() == Some(reg.rel_path(&d).as_str())) {
                        bail!("{} is already the folder of {} ({})", d.display(), other.id, other.slug);
                    }
                    Some(d)
                }
                None => None,
            };
            let id = reg.allocate_id()?;
            let p = Project {
                id: id.clone(),
                slug,
                title,
                kind,
                status,
                priority,
                code,
                venue,
                next_action,
                path: dir.as_ref().map(|d| reg.rel_path(d)),
                repo,
                collaborators,
                tags,
                notes: None,
                created: today(),
                updated: today(),
                deadlines: vec![],
                submissions: vec![],
                links: vec![],
            };
            reg.save(&p)?;
            if let Some(d) = &dir {
                write_id_file(d, &id)?;
            }
            println!("{}  {}", p.id, p.slug);
        }

        Cmd::Ls { kind, status, priority, venue, tag, code, all: show_all } => {
            let venue = venue.map(|v| v.to_lowercase());
            let rows: Vec<&Project> = all
                .iter()
                .filter(|p| show_all || status.is_some() || !p.status.is_closed())
                .filter(|p| kind.is_none_or(|k| p.kind == k))
                .filter(|p| status.is_none_or(|s| p.status == s))
                .filter(|p| priority.is_none_or(|x| p.priority == Some(x)))
                .filter(|p| venue.as_ref().is_none_or(|v| {
                    p.venue.as_deref().unwrap_or("").to_lowercase().contains(v)
                }))
                .filter(|p| tag.as_ref().is_none_or(|t| p.tags.contains(t)))
                .filter(|p| code.as_ref().is_none_or(|c| {
                    p.code.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(c))
                }))
                .collect();
            let mut t = table(&["ID", "SLUG", "KIND", "CODE", "STATUS", "PRI", "VENUE", "NEXT"]);
            for p in &rows {
                t.add_row(vec![
                    p.id.clone(),
                    p.slug.clone(),
                    p.kind.to_string(),
                    p.code.clone().unwrap_or_default(),
                    p.status.to_string(),
                    p.priority.map(|x| x.to_string()).unwrap_or_default(),
                    p.venue.clone().unwrap_or_default(),
                    p.next_action.clone().unwrap_or_default(),
                ]);
            }
            print_table(t, rows.len());
        }

        Cmd::Show { project } => {
            let id = reg.resolve(&mut all, project.as_deref().and_then(key))?.id.clone();
            let p = all.iter().find(|p| p.id == id).unwrap();
            show(&reg, &all, p);
        }

        Cmd::Set { project, assignments } => {
            let snapshot = all.clone();
            let p = reg.resolve(&mut all, key(&project))?;
            for a in &assignments {
                let (field, value) = a
                    .split_once('=')
                    .ok_or_else(|| anyhow!("expected FIELD=VALUE, got '{}'", a))?;
                set_field(&reg, &snapshot, p, field.trim(), value.trim())?;
            }
            p.updated = today();
            reg.save(p)?;
            println!("{}  updated", p.id);
        }

        Cmd::Submit { project, venue, date, note } => {
            let p = reg.resolve(&mut all, key(&project))?;
            p.submissions.push(Submission {
                venue: venue.clone(),
                date: opt_date(date)?,
                outcome: Outcome::Submitted,
                decided: None,
                note,
            });
            p.venue = Some(venue);
            p.status = Status::Submitted;
            p.updated = today();
            reg.save(p)?;
            println!("{}  submitted to {}", p.id, p.venue.as_deref().unwrap_or(""));
        }

        Cmd::Outcome { project, outcome, date, note } => {
            let p = reg.resolve(&mut all, key(&project))?;
            let s = p
                .submissions
                .last_mut()
                .ok_or_else(|| anyhow!("{} has no submission to decide (use `ptrack submit`)", p.id))?;
            s.outcome = outcome;
            if outcome != Outcome::UnderReview {
                s.decided = Some(opt_date(date)?);
            }
            if note.is_some() {
                s.note = note;
            }
            let venue = s.venue.clone();
            p.status = match outcome {
                Outcome::Submitted | Outcome::UnderReview => Status::Submitted,
                Outcome::Revision => Status::Revision,
                Outcome::Accepted => Status::Accepted,
                Outcome::Published => Status::Published,
                // Back in the author's hands, looking for the next venue.
                Outcome::Rejected | Outcome::Withdrawn => Status::Active,
            };
            p.updated = today();
            reg.save(p)?;
            println!("{}  {} at {} (status: {})", p.id, outcome, venue, p.status);
        }

        Cmd::Deadline { project, date, what, rm } => {
            let p = reg.resolve(&mut all, key(&project))?;
            if let Some(n) = rm {
                if n == 0 || n > p.deadlines.len() {
                    bail!("{} has no deadline #{} (see `ptrack show {}`)", p.id, n, p.id);
                }
                let d = p.deadlines.remove(n - 1);
                println!("{}  removed deadline {} {}", p.id, d.date, d.what);
            } else {
                let date = date.ok_or_else(|| anyhow!("give a DATE and a description, or --rm N"))?;
                if what.is_empty() {
                    bail!("give a description after the date");
                }
                let d = Deadline { date: parse_date(&date)?, what: what.join(" ") };
                println!("{}  deadline {} {}", p.id, d.date, d.what);
                p.deadlines.push(d);
                p.deadlines.sort_by_key(|d| d.date);
            }
            p.updated = today();
            reg.save(p)?;
        }

        Cmd::Next { kind } => {
            let mut rows: Vec<&Project> = all
                .iter()
                .filter(|p| !p.status.is_closed() && p.status != Status::Paused)
                .filter(|p| kind.is_none_or(|k| p.kind == k))
                .collect();
            rows.sort_by_key(|p| (p.priority.unwrap_or(Priority::Normal), p.status, p.id.clone()));
            let mut t = table(&["ID", "SLUG", "PRI", "STATUS", "NEXT"]);
            for p in &rows {
                t.add_row(vec![
                    p.id.clone(),
                    p.slug.clone(),
                    p.priority.map(|x| x.to_string()).unwrap_or_default(),
                    p.status.to_string(),
                    p.next_action.clone().unwrap_or_else(|| "(none recorded)".into()),
                ]);
            }
            print_table(t, rows.len());
        }

        Cmd::Due { within } => {
            let limit = today() + chrono::Duration::days(parse_span_days(&within)?);
            let mut rows: Vec<(&Deadline, &Project)> = all
                .iter()
                .filter(|p| !p.status.is_closed())
                .flat_map(|p| p.deadlines.iter().map(move |d| (d, p)))
                .filter(|(d, _)| d.date <= limit)
                .collect();
            rows.sort_by_key(|(d, p)| (d.date, p.id.clone()));
            let mut t = table(&["DATE", "IN", "ID", "SLUG", "WHAT"]);
            for (d, p) in &rows {
                let days = (d.date - today()).num_days();
                let when = match days {
                    0 => "today".to_string(),
                    n if n < 0 => format!("OVERDUE {}d", -n),
                    n => format!("{}d", n),
                };
                t.add_row(vec![d.date.to_string(), when, p.id.clone(), p.slug.clone(), d.what.clone()]);
            }
            print_table(t, rows.len());
        }

        Cmd::Path { project } => {
            let p = reg.resolve(&mut all, key(&project))?;
            let stored = p.path.as_deref().ok_or_else(|| anyhow!("{} has no folder recorded", p.id))?;
            println!("{}", reg.abs_path(stored).display());
        }

        Cmd::Link { project, rel, target, rm } => {
            let target_id = match &target {
                Some(t) => Some(reg.resolve(&mut all, Some(t))?.id.clone()),
                None => None,
            };
            let p = reg.resolve(&mut all, key(&project))?;
            if let Some(n) = rm {
                if n == 0 || n > p.links.len() {
                    bail!("{} has no link #{} (see `ptrack show {}`)", p.id, n, p.id);
                }
                let l = p.links.remove(n - 1);
                println!("{}  removed link {} {}", p.id, l.rel, l.target);
            } else {
                let (Some(rel), Some(target)) = (rel, target_id) else {
                    bail!("give REL and TARGET, or --rm N");
                };
                let rel = slugify(&rel);
                if rel.is_empty() {
                    bail!("relation is empty");
                }
                if target == p.id {
                    bail!("a project cannot link to itself");
                }
                if p.links.iter().any(|l| l.rel == rel && l.target == target) {
                    bail!("{} already {} {}", p.id, rel, target);
                }
                println!("{}  {} {}", p.id, rel, target);
                p.links.push(Link { rel, target });
            }
            p.updated = today();
            reg.save(p)?;
        }

        Cmd::Scan { fix, depth } => scan_cmd(&reg, &mut all, fix, depth)?,

        Cmd::Check { quiet } => check_cmd(&reg, &all, quiet)?,

        Cmd::Export { kind, tag, all: show_all, within, out, json } => {
            let rows: Vec<&Project> = all
                .iter()
                .filter(|p| show_all || !p.status.is_closed())
                .filter(|p| kind.is_none_or(|k| p.kind == k))
                .filter(|p| tag.as_ref().is_none_or(|t| p.tags.contains(t)))
                .collect();
            if json {
                println!("{}", serde_json::to_string(&serde_json::json!({ "today": today(), "projects": rows }))?);
                return Ok(());
            }
            let md = export::markdown(&rows, today(), parse_span_days(&within)?);
            match out {
                None => print!("{md}"),
                Some(f) => {
                    if let Ok(old) = fs::read_to_string(&f)
                        && !old.starts_with(export::MARKER)
                    {
                        bail!("{} exists and was not generated by ptrack; refusing to overwrite", f.display());
                    }
                    fs::write(&f, md)?;
                    println!("wrote {} ({} projects)", f.display(), rows.len());
                }
            }
        }
    }
    Ok(())
}

fn scan_cmd(reg: &Registry, all: &mut [Project], fix: bool, depth: usize) -> Result<()> {
    let found = scan::find_id_files(&reg.root(), depth);
    let mut t = table(&["ID", "SLUG", "PROBLEM", "DETAIL"]);
    let mut n = 0;
    let mut fixed = 0;
    let mut row = |t: &mut Table, id: &str, slug: &str, what: &str, detail: String| {
        t.add_row(vec![id.to_string(), slug.to_string(), what.to_string(), detail]);
        n += 1;
    };

    for (id, dirs) in &found {
        if !all.iter().any(|p| &p.id == id) {
            for d in dirs {
                row(&mut t, id, "", "unknown id", d.display().to_string());
            }
        }
    }

    for p in all.iter_mut() {
        let here: Vec<PathBuf> = found.get(&p.id).cloned().unwrap_or_default();
        let Some(stored) = p.path.clone() else {
            if !here.is_empty() {
                let list: Vec<String> = here.iter().map(|d| reg.rel_path(d)).collect();
                row(&mut t, &p.id, &p.slug, "no path recorded", format!("id file in {}", list.join(", ")));
            }
            continue;
        };
        let abs = reg.abs_path(&stored);
        let elsewhere: Vec<&PathBuf> = here.iter().filter(|d| **d != abs).collect();
        if !abs.is_dir() {
            match elsewhere.as_slice() {
                [new] => {
                    let rel = reg.rel_path(new);
                    row(&mut t, &p.id, &p.slug, "moved", format!("{} -> {}", stored, rel));
                    if fix {
                        p.path = Some(rel);
                        p.updated = today();
                        reg.save(p)?;
                        fixed += 1;
                    }
                }
                [] => row(&mut t, &p.id, &p.slug, "missing", stored.clone()),
                many => {
                    let list: Vec<String> = many.iter().map(|d| reg.rel_path(d)).collect();
                    row(&mut t, &p.id, &p.slug, "missing", format!("{}; id file in {}", stored, list.join(", ")));
                }
            }
            continue;
        }
        if !here.contains(&abs) {
            // Not seen by the walk: either outside the root/depth or absent.
            let id_file = abs.join(ID_FILE);
            match fs::read_to_string(&id_file) {
                Ok(other) if other.trim() == p.id => {}
                Ok(other) => row(&mut t, &p.id, &p.slug, "wrong id file", format!("{} holds {}", stored, other.trim())),
                Err(_) => {
                    row(&mut t, &p.id, &p.slug, "no id file", stored.clone());
                    if fix {
                        write_id_file(&abs, &p.id)?;
                        fixed += 1;
                    }
                }
            }
        }
        for d in elsewhere {
            row(&mut t, &p.id, &p.slug, "duplicate id file", reg.rel_path(d));
        }
    }

    print_table(t, n);
    if fix {
        println!("fixed {}", fixed);
    } else if n > 0 {
        println!("\n`ptrack scan --fix` records moved folders and writes missing id files.");
    }
    Ok(())
}

fn check_cmd(reg: &Registry, all: &[Project], quiet: bool) -> Result<()> {
    let idx = cran::cran_index()?;
    let mut t = table(&["ID", "SLUG", "PACKAGE", "LOCAL", "CRAN", "STATE"]);
    let mut n = 0;
    for p in all.iter().filter(|p| p.kind == Kind::Software && p.status != Status::Archived) {
        let tagged = p.tags.iter().any(|t| t == "cran");
        let Some(stored) = &p.path else { continue };
        let (name, local) = match cran::find_package(&reg.abs_path(stored), &p.slug) {
            Ok(Some(pkg)) => (pkg.name, Some(pkg.version)),
            Ok(None) => {
                if tagged {
                    t.add_row(vec![p.id.clone(), p.slug.clone(), String::new(), String::new(), String::new(), "tagged cran, no DESCRIPTION".into()]);
                    n += 1;
                }
                continue;
            }
            Err(names) => {
                t.add_row(vec![p.id.clone(), p.slug.clone(), names.join(","), String::new(), String::new(), "several packages, none named like the slug".into()]);
                n += 1;
                continue;
            }
        };
        let on_cran = idx.get(&name);
        let state = match (on_cran, local.as_deref()) {
            (None, _) if tagged => "tagged cran, NOT on CRAN",
            (None, _) => "not on CRAN",
            (Some(c), Some(l)) => match cran::cmp_version(l, c) {
                Ordering::Greater => "ahead (unreleased changes)",
                Ordering::Equal => "same",
                Ordering::Less => "BEHIND CRAN",
            },
            (Some(_), None) => "on CRAN",
        };
        let state = if on_cran.is_some() && !tagged { format!("{state}; untagged") } else { state.to_string() };
        let fine = state == "same" || state == "not on CRAN" || state.starts_with("ahead");
        if quiet && fine {
            continue;
        }
        t.add_row(vec![
            p.id.clone(),
            p.slug.clone(),
            name,
            local.unwrap_or_default(),
            on_cran.cloned().unwrap_or_default(),
            state,
        ]);
        n += 1;
    }
    print_table(t, n);
    Ok(())
}

fn check_slug(all: &[Project], slug: &str, except_id: Option<&str>) -> Result<()> {
    if slug.is_empty() {
        bail!("slug is empty");
    }
    if slug.len() == 5 && slug.starts_with('p') && slug[1..].chars().all(|c| c.is_ascii_digit()) {
        bail!("slug '{}' looks like an id", slug);
    }
    if slug != slugify(slug) {
        bail!("slug '{}' must be lowercase letters, digits and '-' (try '{}')", slug, slugify(slug));
    }
    if let Some(o) = all.iter().find(|p| p.slug == slug && Some(p.id.as_str()) != except_id) {
        bail!("slug '{}' is already used by {}", slug, o.id);
    }
    Ok(())
}

fn none_if_empty(v: &str) -> Option<String> {
    if v.is_empty() { None } else { Some(v.to_string()) }
}

fn split_list(v: &str) -> Vec<String> {
    v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
}

fn set_field(reg: &Registry, all: &[Project], p: &mut Project, field: &str, v: &str) -> Result<()> {
    match field {
        "title" => {
            if v.is_empty() { bail!("title cannot be empty"); }
            p.title = v.to_string();
        }
        "slug" => {
            check_slug(all, v, Some(&p.id))?;
            p.slug = v.to_string();
        }
        "kind" => p.kind = v.parse()?,
        "status" => p.status = v.parse()?,
        "priority" => p.priority = if v.is_empty() { None } else { Some(v.parse()?) },
        "code" => p.code = none_if_empty(v),
        "venue" => p.venue = none_if_empty(v),
        "next" | "next_action" => p.next_action = none_if_empty(v),
        "repo" => p.repo = none_if_empty(v),
        "notes" => p.notes = none_if_empty(v),
        "collaborators" | "collab" => p.collaborators = split_list(v),
        "tags" => p.tags = split_list(v),
        "path" => {
            if v.is_empty() {
                p.path = None;
            } else {
                let d = PathBuf::from(v)
                    .canonicalize()
                    .map_err(|_| anyhow!("folder {} does not exist", v))?;
                let rel = reg.rel_path(&d);
                if let Some(o) = all.iter().find(|o| o.id != p.id && o.path.as_deref() == Some(rel.as_str())) {
                    bail!("{} is already the folder of {} ({})", d.display(), o.id, o.slug);
                }
                write_id_file(&d, &p.id)?;
                p.path = Some(rel);
            }
        }
        "id" | "created" | "updated" => bail!("'{}' cannot be changed", field),
        _ => bail!(
            "unknown field '{}' (title slug kind status priority code venue next path repo notes collaborators tags)",
            field
        ),
    }
    Ok(())
}

fn show(reg: &Registry, all: &[Project], p: &Project) {
    let line = |k: &str, v: &str| println!("{:<14}{}", k, v);
    line("id", &p.id);
    line("slug", &p.slug);
    line("title", &p.title);
    line("kind", p.kind.as_str());
    line("status", p.status.as_str());
    if let Some(x) = p.priority { line("priority", x.as_str()); }
    if let Some(v) = &p.code { line("code", v); }
    if let Some(v) = &p.venue { line("venue", v); }
    if let Some(v) = &p.next_action { line("next", v); }
    if let Some(v) = &p.path {
        let abs = reg.abs_path(v);
        let flag = if abs.is_dir() { "" } else { "  (MISSING)" };
        line("path", &format!("{}{}", abs.display(), flag));
    }
    if let Some(v) = &p.repo { line("repo", v); }
    if !p.collaborators.is_empty() { line("collaborators", &p.collaborators.join(", ")); }
    if !p.tags.is_empty() { line("tags", &p.tags.join(", ")); }
    if let Some(v) = &p.notes { line("notes", v); }
    line("created", &p.created.to_string());
    line("updated", &p.updated.to_string());
    if !p.deadlines.is_empty() {
        println!("\ndeadlines");
        for (i, d) in p.deadlines.iter().enumerate() {
            println!("  {}. {}  {}", i + 1, d.date, d.what);
        }
    }
    if !p.submissions.is_empty() {
        println!("\nsubmissions");
        for s in &p.submissions {
            let decided = s.decided.map(|d| format!(" ({})", d)).unwrap_or_default();
            let note = s.note.as_deref().map(|n| format!("  {}", n)).unwrap_or_default();
            println!("  {}  {}  {}{}{}", s.date, s.venue, s.outcome, decided, note);
        }
    }
    let slug_of = |id: &str| all.iter().find(|o| o.id == id).map(|o| o.slug.clone()).unwrap_or_else(|| "(unknown)".into());
    if !p.links.is_empty() {
        println!("\nlinks");
        for (i, l) in p.links.iter().enumerate() {
            println!("  {}. {} {} {}", i + 1, l.rel, l.target, slug_of(&l.target));
        }
    }
    let incoming: Vec<(&Project, &Link)> = all
        .iter()
        .flat_map(|o| o.links.iter().map(move |l| (o, l)))
        .filter(|(_, l)| l.target == p.id)
        .collect();
    if !incoming.is_empty() {
        println!("\nlinked from");
        for (o, l) in incoming {
            println!("  {} {} {}", o.id, o.slug, l.rel);
        }
    }
}

fn table(header: &[&str]) -> Table {
    let mut t = Table::new();
    t.load_style(presets::NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(header.to_vec());
    t
}

fn print_table(t: Table, n: usize) {
    if n == 0 {
        println!("(nothing)");
    } else {
        println!("{t}");
    }
}
