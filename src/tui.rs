use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::{env, fs};

use anyhow::{Context, Result, anyhow, bail};
use chrono::NaiveDate;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEventKind,
    },
    execute,
};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table, TableState, Wrap,
    },
};
use serde::{Deserialize, Serialize};

use crate::model::{Kind, Priority, Project, Status, parse_date, slugify};
use crate::registry::Registry;
use crate::{check_slug, set_field, today};

const COLUMNS: &[&str] = &[
    "id", "pri", "status", "kind", "code", "due", "slug", "title", "venue", "tags", "updated", "next",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum SortKey {
    Priority,
    Deadline,
    Status,
    Updated,
    Id,
    Slug,
    Code,
}

impl SortKey {
    const ALL: &'static [SortKey] = &[
        SortKey::Priority,
        SortKey::Deadline,
        SortKey::Status,
        SortKey::Updated,
        SortKey::Id,
        SortKey::Slug,
        SortKey::Code,
    ];
    fn as_str(self) -> &'static str {
        match self {
            SortKey::Priority => "priority",
            SortKey::Deadline => "deadline",
            SortKey::Status => "status",
            SortKey::Updated => "updated",
            SortKey::Id => "id",
            SortKey::Slug => "slug",
            SortKey::Code => "code",
        }
    }
}

/// Saved view: ~/.config/ptrack/browse.toml (or $PTRACK_BROWSE_CONFIG).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    /// List columns, in display order (see COLUMNS)
    columns: Vec<String>,
    sort: SortKey,
    reverse: bool,
    /// Also show published/archived projects and the `hide` statuses
    show_all: bool,
    /// Statuses hidden from the default view, on top of published/archived
    hide: Vec<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<Kind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    priority: Option<Priority>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    /// Tags that `t` cycles through (default: the most used tags)
    quick_tags: Vec<String>,
    /// Tags offered by `N` as "kind [tag]", e.g. a portfolio that spans kinds
    new_groups: Vec<String>,
    /// Detail pane beside the list
    split: bool,
    /// Detail pane width, percent of the screen
    detail_width: u16,
    /// Deadlines this close are highlighted
    soon_days: i64,
    mouse: bool,
    /// Overrides, e.g. "status.active" = "green", "accent" = "#89b4fa"
    colors: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            columns: ["id", "pri", "status", "kind", "due", "slug", "next"]
                .map(String::from)
                .to_vec(),
            sort: SortKey::Priority,
            reverse: false,
            show_all: false,
            hide: vec![],
            kind: None,
            status: None,
            priority: None,
            tag: None,
            code: None,
            quick_tags: vec![],
            new_groups: vec![],
            split: true,
            detail_width: 40,
            soon_days: 14,
            mouse: true,
            colors: BTreeMap::new(),
        }
    }
}

fn default_colors() -> BTreeMap<String, Color> {
    use Color::*;
    [
        ("status.idea", DarkGray),
        ("status.planned", LightBlue),
        ("status.active", Green),
        ("status.paused", Yellow),
        ("status.submitted", Magenta),
        ("status.revision", LightRed),
        ("status.accepted", Cyan),
        ("status.published", Gray),
        ("status.archived", DarkGray),
        ("priority.high", LightRed),
        ("priority.normal", Reset),
        ("priority.low", DarkGray),
        ("due.overdue", Red),
        ("due.soon", Yellow),
        ("due.later", Reset),
        ("accent", Cyan),
        ("selected", Indexed(237)),
        ("border", DarkGray),
        ("dim", DarkGray),
    ]
    .into_iter()
    .map(|(k, c)| (k.to_string(), c))
    .collect()
}

fn config_path() -> Option<PathBuf> {
    if let Ok(p) = env::var("PTRACK_BROWSE_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let base = env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|_| env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok()?;
    Some(base.join("ptrack").join("browse.toml"))
}

/// Status order used for sorting: what needs attention first.
fn status_rank(s: Status) -> u8 {
    match s {
        Status::Revision => 0,
        Status::Active => 1,
        Status::Submitted => 2,
        Status::Accepted => 3,
        Status::Planned => 4,
        Status::Idea => 5,
        Status::Paused => 6,
        Status::Published => 7,
        Status::Archived => 8,
    }
}

fn next_due(p: &Project) -> Option<NaiveDate> {
    p.deadlines.iter().map(|d| d.date).min()
}

fn priority_label(p: Option<Priority>) -> &'static str {
    match p {
        Some(Priority::High) => "high",
        Some(Priority::Normal) => "norm",
        Some(Priority::Low) => "low",
        None => "",
    }
}

#[derive(Clone, Copy)]
enum InputKind {
    Next,
    Deadline,
    Field,
    /// Kind, and an index into `new_groups`
    NewTitle(Kind, Option<usize>),
}

#[derive(Clone, Copy)]
enum PickKind {
    FilterField,
    FilterValue(&'static str),
    Status,
    Priority,
    Kind,
    NewKind,
    Columns,
}

enum Mode {
    Normal,
    Search,
    Help,
    Input { what: InputKind, buf: String },
    Pick { what: PickKind, items: Vec<String>, state: ListState },
}

struct App<'a> {
    reg: &'a Registry,
    all: Vec<Project>,
    cfg: Config,
    cfg_path: Option<PathBuf>,
    colors: BTreeMap<String, Color>,
    view: Vec<usize>,
    table: TableState,
    search: String,
    mode: Mode,
    focus_detail: bool,
    full_detail: bool,
    detail_scroll: u16,
    msg: Option<(String, bool)>,
    page: usize,
    list_area: Rect,
    quit: bool,
}

pub fn run(reg: &Registry, all: Vec<Project>) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("interactive dashboard requires a terminal; use `ptrack ls --all` for plain output");
    }
    let mut app = App::new(reg, all);
    let mut terminal = ratatui::init();
    if app.cfg.mouse {
        let _ = execute!(io::stdout(), EnableMouseCapture);
    }
    let result = app.event_loop(&mut terminal);
    if app.cfg.mouse {
        let _ = execute!(io::stdout(), DisableMouseCapture);
    }
    ratatui::restore();
    result
}

impl<'a> App<'a> {
    fn new(reg: &'a Registry, all: Vec<Project>) -> Self {
        let cfg_path = config_path();
        let mut problems = vec![];
        let mut cfg = match cfg_path.as_ref().filter(|p| p.is_file()) {
            None => Config::default(),
            Some(p) => match fs::read_to_string(p)
                .map_err(anyhow::Error::from)
                .and_then(|t| toml::from_str::<Config>(&t).map_err(anyhow::Error::from))
            {
                Ok(c) => c,
                Err(e) => {
                    problems.push(format!("{}: {e}; using defaults", p.display()));
                    Config::default()
                }
            },
        };
        cfg.columns.retain(|c| {
            let ok = COLUMNS.contains(&c.as_str());
            if !ok {
                problems.push(format!("unknown column '{c}'"));
            }
            ok
        });
        if cfg.columns.is_empty() {
            cfg.columns = Config::default().columns;
        }
        cfg.detail_width = cfg.detail_width.clamp(20, 80);
        let mut colors = default_colors();
        for (k, v) in &cfg.colors {
            if !colors.contains_key(k) {
                problems.push(format!("unknown color key '{k}'"));
            } else {
                match v.parse::<Color>() {
                    Ok(c) => {
                        colors.insert(k.clone(), c);
                    }
                    Err(_) => problems.push(format!("bad color '{v}' for {k}")),
                }
            }
        }
        let mut app = App {
            reg,
            all,
            cfg,
            cfg_path,
            colors,
            view: vec![],
            table: TableState::default(),
            search: String::new(),
            mode: Mode::Normal,
            focus_detail: false,
            full_detail: false,
            detail_scroll: 0,
            msg: None,
            page: 10,
            list_area: Rect::default(),
            quit: false,
        };
        if !problems.is_empty() {
            app.msg = Some((format!("config: {}", problems.join("; ")), true));
        }
        app.refresh();
        app
    }

    fn color(&self, key: &str) -> Color {
        self.colors.get(key).copied().unwrap_or(Color::Reset)
    }

    fn fg(&self, key: &str) -> Style {
        Style::new().fg(self.color(key))
    }

    fn current(&self) -> Option<usize> {
        self.table.selected().and_then(|i| self.view.get(i).copied())
    }

    fn visible(&self, p: &Project, terms: &[String]) -> bool {
        let c = &self.cfg;
        let shown = c.show_all
            || c.status.is_some()
            || (!p.status.is_closed() && !c.hide.contains(&p.status));
        shown
            && c.kind.is_none_or(|k| p.kind == k)
            && c.status.is_none_or(|s| p.status == s)
            && c.priority.is_none_or(|x| p.priority == Some(x))
            && c.tag.as_ref().is_none_or(|t| p.tags.contains(t))
            && c.code.as_ref().is_none_or(|x| p.code.as_ref() == Some(x))
            && (terms.is_empty() || {
                let hay = [
                    p.id.as_str(),
                    p.code.as_deref().unwrap_or(""),
                    &p.slug,
                    &p.title,
                    p.next_action.as_deref().unwrap_or(""),
                    p.venue.as_deref().unwrap_or(""),
                    p.notes.as_deref().unwrap_or(""),
                    &p.tags.join(" "),
                    &p.collaborators.join(" "),
                ]
                .join(" ")
                .to_lowercase();
                terms.iter().all(|t| hay.contains(t.as_str()))
            })
    }

    fn compare(&self, a: &Project, b: &Project) -> Ordering {
        let pri = |p: &Project| p.priority.unwrap_or(Priority::Normal);
        let due = |p: &Project| next_due(p).unwrap_or(NaiveDate::MAX);
        let rank = |p: &Project| status_rank(p.status);
        match self.cfg.sort {
            SortKey::Priority => pri(a)
                .cmp(&pri(b))
                .then(rank(a).cmp(&rank(b)))
                .then(due(a).cmp(&due(b))),
            SortKey::Deadline => due(a).cmp(&due(b)).then(pri(a).cmp(&pri(b))),
            SortKey::Status => rank(a).cmp(&rank(b)).then(pri(a).cmp(&pri(b))),
            SortKey::Updated => b.updated.cmp(&a.updated),
            SortKey::Id => Ordering::Equal,
            SortKey::Slug => a.slug.cmp(&b.slug),
            SortKey::Code => match (&a.code, &b.code) {
                (Some(x), Some(y)) => x.cmp(y),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
            .then(pri(a).cmp(&pri(b))),
        }
        .then_with(|| a.id.cmp(&b.id))
    }

    /// Recomputes the filtered, sorted view, keeping the selected project when it is still shown.
    fn refresh(&mut self) {
        let keep = self.current().map(|i| self.all[i].id.clone());
        let old_pos = self.table.selected().unwrap_or(0);
        let terms: Vec<String> = self
            .search
            .to_lowercase()
            .split_whitespace()
            .map(String::from)
            .collect();
        let mut view: Vec<usize> = (0..self.all.len())
            .filter(|&i| self.visible(&self.all[i], &terms))
            .collect();
        view.sort_by(|&a, &b| self.compare(&self.all[a], &self.all[b]));
        if self.cfg.reverse {
            view.reverse();
        }
        self.view = view;
        let pos = keep
            .and_then(|id| self.view.iter().position(|&i| self.all[i].id == id))
            .unwrap_or(old_pos.min(self.view.len().saturating_sub(1)));
        self.table
            .select(if self.view.is_empty() { None } else { Some(pos) });
    }

    fn select(&mut self, pos: usize) {
        if !self.view.is_empty() {
            let pos = pos.min(self.view.len() - 1);
            if self.table.selected() != Some(pos) {
                self.detail_scroll = 0;
            }
            self.table.select(Some(pos));
        }
    }

    fn step(&mut self, delta: isize) {
        let cur = self.table.selected().unwrap_or(0) as isize;
        self.select((cur + delta).max(0) as usize);
    }

    fn flash(&mut self, text: impl Into<String>) {
        self.msg = Some((text.into(), false));
    }

    fn fail(&mut self, e: anyhow::Error) {
        self.msg = Some((format!("{e:#}"), true));
    }

    /// Applies `f` to a copy of the selected project and saves it to the registry.
    fn edit(&mut self, f: impl FnOnce(&Registry, &[Project], &mut Project) -> Result<String>) {
        let Some(i) = self.current() else { return };
        let reg = self.reg;
        let mut p = self.all[i].clone();
        let result = f(reg, &self.all, &mut p).and_then(|m| {
            p.updated = today();
            reg.save(&p)?;
            Ok(m)
        });
        match result {
            Ok(m) => {
                self.flash(format!("{}  {m}", p.id));
                self.all[i] = p;
                self.refresh();
            }
            Err(e) => self.fail(e),
        }
    }

    fn reload(&mut self) {
        match self.reg.load_all() {
            Ok(all) => {
                self.all = all;
                self.refresh();
                self.flash(format!("reloaded {} projects", self.all.len()));
            }
            Err(e) => self.fail(e),
        }
    }

    fn save_config(&mut self) {
        let result = (|| -> Result<PathBuf> {
            let path = self
                .cfg_path
                .clone()
                .ok_or_else(|| anyhow!("no config location (HOME unset)"))?;
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            let keys: Vec<&str> = self.colors.keys().map(String::as_str).collect();
            let text = format!(
                "# ptrack browse view; written by `w` in the dashboard\n\
                 # columns: {}\n\
                 # sort: priority deadline status updated id slug code\n\
                 # [colors] keys: {}\n\
                 # values: names (green, light-blue, dark-gray), \"#rrggbb\" or 0-255\n{}",
                COLUMNS.join(" "),
                keys.join(" "),
                toml::to_string_pretty(&self.cfg)?
            );
            fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
            Ok(path)
        })();
        match result {
            Ok(p) => self.flash(format!("view saved to {}", p.display())),
            Err(e) => self.fail(e),
        }
    }

    fn open_pick(&mut self, what: PickKind) {
        let cur = self.current().map(|i| &self.all[i]);
        let (items, at): (Vec<String>, usize) = match what {
            PickKind::FilterField => (
                ["status", "kind", "priority", "tag", "code", "clear all filters"]
                    .map(String::from)
                    .to_vec(),
                0,
            ),
            PickKind::FilterValue(field) => {
                let mut items = vec!["(any)".to_string()];
                match field {
                    "status" => items.extend(Status::ALL.iter().map(|s| s.to_string())),
                    "kind" => {
                        items.extend(Kind::ALL.iter().map(|s| s.to_string()));
                        for g in &self.cfg.new_groups {
                            let n = self.all.iter().filter(|p| p.tags.contains(g)).count();
                            items.push(format!("[{g}]  ({n})"));
                        }
                    }
                    "priority" => items.extend(Priority::ALL.iter().map(|s| s.to_string())),
                    "code" => {
                        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
                        for c in self.all.iter().filter_map(|p| p.code.as_deref()) {
                            *counts.entry(c).or_default() += 1;
                        }
                        items.extend(counts.into_iter().map(|(c, n)| format!("{c}  ({n})")));
                    }
                    _ => {
                        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
                        for t in self.all.iter().flat_map(|p| &p.tags) {
                            *counts.entry(t).or_default() += 1;
                        }
                        items.extend(counts.into_iter().map(|(t, n)| format!("{t}  ({n})")));
                    }
                }
                (items, 0)
            }
            PickKind::Status => {
                let Some(p) = cur else { return };
                let at = Status::ALL.iter().position(|s| *s == p.status).unwrap_or(0);
                (Status::ALL.iter().map(|s| s.to_string()).collect(), at)
            }
            PickKind::Kind => {
                let Some(p) = cur else { return };
                let at = Kind::ALL.iter().position(|s| *s == p.kind).unwrap_or(0);
                (Kind::ALL.iter().map(|s| s.to_string()).collect(), at)
            }
            PickKind::Priority => {
                let Some(p) = cur else { return };
                let mut items: Vec<String> = Priority::ALL.iter().map(|s| s.to_string()).collect();
                items.push("(unset)".into());
                let at = p
                    .priority
                    .and_then(|x| Priority::ALL.iter().position(|s| *s == x))
                    .unwrap_or(Priority::ALL.len());
                (items, at)
            }
            PickKind::NewKind => {
                let at = self.cfg.kind.and_then(|k| Kind::ALL.iter().position(|x| *x == k)).unwrap_or(0);
                let mut items: Vec<String> = Kind::ALL.iter().map(|s| s.to_string()).collect();
                for g in &self.cfg.new_groups {
                    items.extend(Kind::ALL.iter().map(|k| format!("{k} [{g}]")));
                }
                (items, at)
            }
            PickKind::Columns => (self.column_items(), 0),
        };
        self.mode = Mode::Pick {
            what,
            items,
            state: ListState::default().with_selected(Some(at)),
        };
    }

    /// Shown columns in order, then hidden ones.
    fn column_items(&self) -> Vec<String> {
        let mut items: Vec<String> = self.cfg.columns.clone();
        items.extend(
            COLUMNS
                .iter()
                .filter(|c| !self.cfg.columns.iter().any(|x| x == *c))
                .map(|c| c.to_string()),
        );
        items
    }

    fn picked(&mut self, what: PickKind, item: String) {
        let value = item.split("  (").next().unwrap_or("").to_string();
        let any = value == "(any)";
        match what {
            PickKind::FilterField => {
                if value == "clear all filters" {
                    self.clear_filters();
                } else {
                    let field = ["status", "kind", "priority", "tag", "code"]
                        .into_iter()
                        .find(|f| *f == value)
                        .unwrap_or("tag");
                    self.open_pick(PickKind::FilterValue(field));
                    return;
                }
            }
            PickKind::FilterValue(field) => {
                let c = &mut self.cfg;
                match field {
                    "status" => c.status = if any { None } else { value.parse().ok() },
                    // `[group]` entries (new_groups) filter by that tag across all kinds
                    "kind" if value.starts_with('[') => {
                        c.kind = None;
                        c.tag = Some(value.trim_matches(['[', ']']).to_string());
                    }
                    "kind" => c.kind = if any { None } else { value.parse().ok() },
                    "priority" => c.priority = if any { None } else { value.parse().ok() },
                    "code" => c.code = if any { None } else { Some(value) },
                    _ => c.tag = if any { None } else { Some(value) },
                }
                self.refresh();
            }
            PickKind::Status => self.edit(|_, _, p| {
                p.status = value.parse()?;
                Ok(format!("status {}", p.status))
            }),
            PickKind::Kind => self.edit(|_, _, p| {
                p.kind = value.parse()?;
                Ok(format!("kind {}", p.kind))
            }),
            PickKind::Priority => self.edit(|_, _, p| {
                p.priority = if value == "(unset)" { None } else { Some(value.parse()?) };
                Ok(format!("priority {}", p.priority.map(|x| x.as_str()).unwrap_or("unset")))
            }),
            PickKind::NewKind => {
                let (kind, group) = match item.split_once(" [") {
                    Some((k, g)) => {
                        let g = g.trim_end_matches(']');
                        (k, self.cfg.new_groups.iter().position(|x| x == g))
                    }
                    None => (item.as_str(), None),
                };
                if let Ok(kind) = kind.parse() {
                    self.mode = Mode::Input { what: InputKind::NewTitle(kind, group), buf: String::new() };
                    return;
                }
            }
            PickKind::Columns => {}
        }
        self.mode = Mode::Normal;
    }

    /// `t`: steps the tag filter through the quick tags, then back to none.
    fn cycle_quick_tag(&mut self) {
        let mut quick = self.cfg.quick_tags.clone();
        if quick.is_empty() {
            let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
            for t in self.all.iter().flat_map(|p| &p.tags) {
                *counts.entry(t).or_default() += 1;
            }
            let mut by_use: Vec<(&str, usize)> = counts.into_iter().collect();
            by_use.sort_by(|a, b| b.1.cmp(&a.1));
            quick = by_use.into_iter().take(5).map(|(t, _)| t.to_string()).collect();
        }
        if quick.is_empty() {
            self.flash("no tags in the registry");
            return;
        }
        let next = match self.cfg.tag.as_ref().and_then(|t| quick.iter().position(|q| q == t)) {
            Some(i) if i + 1 < quick.len() => Some(quick[i + 1].clone()),
            Some(_) => None,
            None => Some(quick[0].clone()),
        };
        self.flash(match &next {
            Some(t) => format!("tag view: {t}"),
            None => "tag view off".into(),
        });
        self.cfg.tag = next;
        self.refresh();
    }

    fn clear_filters(&mut self) {
        let c = &mut self.cfg;
        c.kind = None;
        c.status = None;
        c.priority = None;
        c.tag = None;
        c.code = None;
        self.search.clear();
        self.refresh();
        self.flash("filters cleared");
    }

    fn submit_input(&mut self, what: InputKind, buf: String) {
        let buf = buf.trim().to_string();
        match what {
            InputKind::Next => self.edit(|_, _, p| {
                p.next_action = if buf.is_empty() { None } else { Some(buf) };
                Ok("next action updated".into())
            }),
            InputKind::Deadline => self.edit(|_, _, p| {
                let (date, what) = buf.split_once(' ').unwrap_or((&buf, ""));
                let what = what.trim();
                if what.is_empty() {
                    bail!("expected: YYYY-MM-DD description");
                }
                let d = crate::model::Deadline { date: parse_date(date)?, what: what.into() };
                let m = format!("deadline {} {}", d.date, d.what);
                p.deadlines.push(d);
                p.deadlines.sort_by_key(|d| d.date);
                Ok(m)
            }),
            InputKind::Field => self.edit(|reg, all, p| {
                let (field, value) = buf
                    .split_once('=')
                    .ok_or_else(|| anyhow!("expected FIELD=VALUE"))?;
                set_field(reg, all, p, field.trim(), value.trim())?;
                Ok(format!("{} updated", field.trim()))
            }),
            InputKind::NewTitle(kind, group) => {
                if let Err(e) = self.create(kind, group, buf) {
                    self.fail(e);
                }
            }
        }
    }

    /// `N`: adds a project with the given kind and title. Status, priority, tag and
    /// code come from the active filters, so the project lands in the current view;
    /// `group` adds one of the `new_groups` tags.
    fn create(&mut self, kind: Kind, group: Option<usize>, title: String) -> Result<()> {
        if title.is_empty() {
            bail!("title is empty");
        }
        let slug = unique_slug(&self.all, &slugify(&title));
        check_slug(&self.all, &slug, None)?;
        let id = self.reg.allocate_id()?;
        let c = &self.cfg;
        let mut tags: Vec<String> = c.tag.iter().cloned().collect();
        if let Some(g) = group.and_then(|g| c.new_groups.get(g)) {
            if !tags.contains(g) {
                tags.push(g.clone());
            }
        }
        let p = Project {
            id,
            slug,
            title,
            kind,
            status: c.status.unwrap_or(Status::Planned),
            priority: c.priority,
            code: c.code.clone(),
            venue: None,
            next_action: None,
            path: None,
            repo: None,
            collaborators: vec![],
            tags,
            notes: None,
            created: today(),
            updated: today(),
            deadlines: vec![],
            submissions: vec![],
            links: vec![],
        };
        self.reg.save(&p)?;
        let id = p.id.clone();
        self.flash(format!("{}  {} created ({})", p.id, p.slug, p.kind));
        self.all.push(p);
        self.refresh();
        match self.view.iter().position(|&i| self.all[i].id == id) {
            Some(pos) => self.select(pos),
            None => self.flash(format!("{id} created but hidden by the current search or view")),
        }
        Ok(())
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            terminal.draw(|f| self.draw(f))?;
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k),
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::ScrollDown if self.focus_detail || self.full_detail => {
                        self.detail_scroll = self.detail_scroll.saturating_add(3)
                    }
                    MouseEventKind::ScrollUp if self.focus_detail || self.full_detail => {
                        self.detail_scroll = self.detail_scroll.saturating_sub(3)
                    }
                    MouseEventKind::ScrollDown => self.step(3),
                    MouseEventKind::ScrollUp => self.step(-3),
                    MouseEventKind::Down(MouseButton::Left) => {
                        let a = self.list_area;
                        // Rows start below the top border and the header row.
                        if !self.full_detail
                            && m.column >= a.x
                            && m.column < a.x + a.width
                            && m.row >= a.y + 2
                            && m.row < a.y + a.height.saturating_sub(1)
                        {
                            let pos = self.table.offset() + usize::from(m.row - a.y - 2);
                            if pos < self.view.len() {
                                self.focus_detail = false;
                                self.select(pos);
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        Ok(())
    }

    fn on_key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.on_normal_key(k),
            Mode::Help => {}
            Mode::Search => match k.code {
                KeyCode::Enter => {}
                KeyCode::Esc => {
                    self.search.clear();
                    self.refresh();
                }
                KeyCode::Backspace => {
                    self.search.pop();
                    self.refresh();
                    self.mode = Mode::Search;
                }
                KeyCode::Char(c) => {
                    self.search.push(c);
                    self.refresh();
                    self.mode = Mode::Search;
                }
                _ => self.mode = Mode::Search,
            },
            Mode::Input { what, mut buf } => match k.code {
                KeyCode::Enter => self.submit_input(what, buf),
                KeyCode::Esc => {}
                code => {
                    match code {
                        KeyCode::Backspace => {
                            buf.pop();
                        }
                        KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                            buf.clear()
                        }
                        KeyCode::Char(c) => buf.push(c),
                        _ => {}
                    }
                    self.mode = Mode::Input { what, buf };
                }
            },
            Mode::Pick { what, mut items, mut state } => {
                let n = items.len();
                let at = state.selected().unwrap_or(0);
                let columns = matches!(what, PickKind::Columns);
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') => return,
                    KeyCode::Down | KeyCode::Char('j') => state.select(Some((at + 1).min(n - 1))),
                    KeyCode::Up | KeyCode::Char('k') => state.select(Some(at.saturating_sub(1))),
                    KeyCode::Char(' ') | KeyCode::Enter if columns => {
                        let name = items[at].clone();
                        if let Some(i) = self.cfg.columns.iter().position(|c| *c == name) {
                            if self.cfg.columns.len() > 1 {
                                self.cfg.columns.remove(i);
                            }
                        } else {
                            self.cfg.columns.push(name.clone());
                        }
                        items = self.column_items();
                        state.select(items.iter().position(|c| *c == name));
                    }
                    KeyCode::Char('J') | KeyCode::Char('K') if columns => {
                        let shown = self.cfg.columns.len();
                        let down = k.code == KeyCode::Char('J');
                        if at < shown && ((down && at + 1 < shown) || (!down && at > 0)) {
                            let to = if down { at + 1 } else { at - 1 };
                            self.cfg.columns.swap(at, to);
                            items = self.column_items();
                            state.select(Some(to));
                        }
                    }
                    KeyCode::Enter => {
                        let item = items[at].clone();
                        self.picked(what, item);
                        return;
                    }
                    _ => {}
                }
                self.mode = Mode::Pick { what, items, state };
            }
        }
    }

    fn on_normal_key(&mut self, k: KeyEvent) {
        self.msg = None;
        let detail = self.focus_detail || self.full_detail;
        let page = self.page as isize;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let scroll = |s: &mut u16, d: isize| *s = (*s as isize + d).clamp(0, u16::MAX as isize) as u16;
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('d') if ctrl => self.step(page / 2),
            KeyCode::Char('u') if ctrl => self.step(-page / 2),
            KeyCode::Down | KeyCode::Char('j') if detail => scroll(&mut self.detail_scroll, 1),
            KeyCode::Up | KeyCode::Char('k') if detail => scroll(&mut self.detail_scroll, -1),
            KeyCode::PageDown if detail => scroll(&mut self.detail_scroll, page),
            KeyCode::PageUp if detail => scroll(&mut self.detail_scroll, -page),
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::PageDown => self.step(page),
            KeyCode::PageUp => self.step(-page),
            KeyCode::Home | KeyCode::Char('g') => self.select(0),
            KeyCode::End | KeyCode::Char('G') => self.select(usize::MAX),
            KeyCode::Tab => {
                self.focus_detail = !self.focus_detail && self.cfg.split;
            }
            KeyCode::Enter => {
                self.full_detail = !self.full_detail;
                self.detail_scroll = 0;
            }
            KeyCode::Esc => {
                if self.full_detail || self.focus_detail {
                    self.full_detail = false;
                    self.focus_detail = false;
                } else if !self.search.is_empty() {
                    self.search.clear();
                    self.refresh();
                }
            }
            KeyCode::Char('v') => {
                self.cfg.split = !self.cfg.split;
                self.focus_detail = false;
            }
            KeyCode::Char('<') => self.cfg.detail_width = (self.cfg.detail_width + 5).min(80),
            KeyCode::Char('>') => self.cfg.detail_width = self.cfg.detail_width.saturating_sub(5).max(20),
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Char('f') => self.open_pick(PickKind::FilterField),
            KeyCode::Char('F') => self.clear_filters(),
            KeyCode::Char('t') => self.cycle_quick_tag(),
            KeyCode::Char('a') => {
                self.cfg.show_all = !self.cfg.show_all;
                self.refresh();
            }
            KeyCode::Char('s') => {
                let i = SortKey::ALL.iter().position(|s| *s == self.cfg.sort).unwrap_or(0);
                self.cfg.sort = SortKey::ALL[(i + 1) % SortKey::ALL.len()];
                self.refresh();
            }
            KeyCode::Char('r') => {
                self.cfg.reverse = !self.cfg.reverse;
                self.refresh();
            }
            KeyCode::Char('c') => self.open_pick(PickKind::Columns),
            KeyCode::Char('S') => self.open_pick(PickKind::Status),
            KeyCode::Char('P') => self.open_pick(PickKind::Priority),
            KeyCode::Char('K') => self.open_pick(PickKind::Kind),
            KeyCode::Char('n') => {
                if let Some(i) = self.current() {
                    let buf = self.all[i].next_action.clone().unwrap_or_default();
                    self.mode = Mode::Input { what: InputKind::Next, buf };
                }
            }
            KeyCode::Char('D') if self.current().is_some() => {
                self.mode = Mode::Input { what: InputKind::Deadline, buf: String::new() }
            }
            KeyCode::Char('e') if self.current().is_some() => {
                self.mode = Mode::Input { what: InputKind::Field, buf: String::new() }
            }
            KeyCode::Char('N') => self.open_pick(PickKind::NewKind),
            KeyCode::Char('R') => self.reload(),
            KeyCode::Char('w') => self.save_config(),
            _ => {}
        }
    }

    // ---------- drawing ----------

    fn draw(&mut self, f: &mut Frame) {
        let [head, chips, body, foot] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(f.area());
        self.draw_header(f, head);
        self.draw_chips(f, chips);
        self.page = usize::from(body.height.saturating_sub(3)).max(1);
        if self.full_detail {
            self.draw_detail(f, body, true);
        } else if self.cfg.split && body.width >= 90 {
            let [list, detail] = Layout::horizontal([
                Constraint::Fill(1),
                Constraint::Percentage(self.cfg.detail_width),
            ])
            .areas(body);
            self.draw_list(f, list);
            self.draw_detail(f, detail, self.focus_detail);
        } else {
            self.draw_list(f, body);
        }
        self.draw_footer(f, foot);
        match &self.mode {
            Mode::Help => self.draw_help(f),
            Mode::Pick { .. } => self.draw_pick(f),
            _ => {}
        }
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let c = &self.cfg;
        let accent = self.color("accent");
        let dim = self.fg("dim");
        let mut spans = vec![
            Span::styled(" ptrack ", Style::new().fg(Color::Black).bg(accent).add_modifier(Modifier::BOLD)),
            Span::raw(format!("  {} / {} shown", self.view.len(), self.all.len())),
            Span::styled("  │  sort ", dim),
            Span::styled(
                format!("{}{}", c.sort.as_str(), if c.reverse { " ↑" } else { " ↓" }),
                Style::new().fg(accent),
            ),
        ];
        let mut filters = vec![];
        if let Some(x) = c.status { filters.push(format!("status={x}")); }
        if let Some(x) = c.kind { filters.push(format!("kind={x}")); }
        if let Some(x) = c.priority { filters.push(format!("priority={x}")); }
        if let Some(x) = &c.tag { filters.push(format!("tag={x}")); }
        if let Some(x) = &c.code { filters.push(format!("code={x}")); }
        if c.show_all { filters.push("all".into()); }
        if !filters.is_empty() {
            spans.push(Span::styled("  │  filter ", dim));
            spans.push(Span::styled(filters.join(" "), Style::new().fg(Color::Yellow)));
        }
        if !self.search.is_empty() {
            spans.push(Span::styled("  │  search ", dim));
            spans.push(Span::styled(format!("“{}”", self.search), Style::new().fg(Color::Yellow)));
        }
        spans.push(Span::styled(format!("  │  {}", today()), dim));
        f.render_widget(Line::from(spans), area);
    }

    fn draw_chips(&self, f: &mut Frame, area: Rect) {
        let mut counts: BTreeMap<u8, (Status, usize)> = BTreeMap::new();
        let mut overdue = 0;
        let mut soon = 0;
        for &i in &self.view {
            let p = &self.all[i];
            counts.entry(status_rank(p.status)).or_insert((p.status, 0)).1 += 1;
            if let Some(d) = next_due(p) {
                let days = (d - today()).num_days();
                if days < 0 { overdue += 1 } else if days <= self.cfg.soon_days { soon += 1 }
            }
        }
        let mut spans = vec![Span::raw(" ")];
        for (s, n) in counts.values() {
            spans.push(Span::styled("● ", self.fg(&format!("status.{s}"))));
            spans.push(Span::raw(format!("{s} {n}   ")));
        }
        if overdue > 0 {
            spans.push(Span::styled(format!("▲ {overdue} overdue   "), self.fg("due.overdue").add_modifier(Modifier::BOLD)));
        }
        if soon > 0 {
            spans.push(Span::styled(format!("◆ {soon} due ≤{}d", self.cfg.soon_days), self.fg("due.soon")));
        }
        f.render_widget(Line::from(spans), area);
    }

    fn due_cell(&self, p: &Project) -> Cell<'static> {
        let Some(d) = next_due(p) else { return Cell::from("") };
        let days = (d - today()).num_days();
        let (text, key) = match days {
            n if n < 0 => (format!("{n}d"), "due.overdue"),
            0 => ("today".into(), "due.overdue"),
            n if n <= self.cfg.soon_days => (format!("{n}d"), "due.soon"),
            n => (format!("{n}d"), "due.later"),
        };
        Cell::from(text).style(self.fg(key))
    }

    fn draw_list(&mut self, f: &mut Frame, area: Rect) {
        self.list_area = area;
        let dim = self.fg("dim");
        let slug_w = self
            .view
            .iter()
            .map(|&i| self.all[i].slug.chars().count())
            .max()
            .unwrap_or(4)
            .clamp(4, 24) as u16;
        let has_fill = self.cfg.columns.iter().any(|c| c == "title" || c == "next");
        let widths: Vec<Constraint> = self
            .cfg
            .columns
            .iter()
            .map(|c| match c.as_str() {
                "id" => Constraint::Length(5),
                "pri" => Constraint::Length(4),
                "status" => Constraint::Length(9),
                "kind" => Constraint::Length(10),
                "code" => Constraint::Length(9),
                "due" => Constraint::Length(6),
                "slug" if !has_fill => Constraint::Fill(1),
                "slug" => Constraint::Length(slug_w),
                "title" => Constraint::Fill(2),
                "venue" => Constraint::Length(16),
                "tags" => Constraint::Length(16),
                "updated" => Constraint::Length(10),
                _ => Constraint::Fill(3),
            })
            .collect();
        let header = Row::new(self.cfg.columns.iter().map(|c| {
            Cell::from(c.to_uppercase()).style(self.fg("accent").add_modifier(Modifier::BOLD))
        }));
        let rows: Vec<Row> = self
            .view
            .iter()
            .map(|&i| {
                let p = &self.all[i];
                Row::new(self.cfg.columns.iter().map(|c| match c.as_str() {
                    "id" => Cell::from(p.id.clone()).style(dim),
                    "pri" => Cell::from(priority_label(p.priority)).style(self.fg(&format!(
                        "priority.{}",
                        p.priority.unwrap_or(Priority::Normal)
                    ))),
                    "status" => Cell::from(p.status.as_str()).style(self.fg(&format!("status.{}", p.status))),
                    "kind" => Cell::from(p.kind.as_str()),
                    "code" => Cell::from(p.code.clone().unwrap_or_default()).style(self.fg("accent")),
                    "due" => self.due_cell(p),
                    "slug" => Cell::from(p.slug.clone()).style(Style::new().add_modifier(Modifier::BOLD)),
                    "title" => Cell::from(p.title.clone()),
                    "venue" => Cell::from(p.venue.clone().unwrap_or_default()),
                    "tags" => Cell::from(p.tags.join(",")).style(dim),
                    "updated" => Cell::from(p.updated.to_string()).style(dim),
                    _ => match &p.next_action {
                        Some(n) => Cell::from(n.clone()),
                        None => Cell::from("·").style(dim),
                    },
                }))
            })
            .collect();
        let border = if self.focus_detail { self.fg("border") } else { self.fg("accent") };
        let table = Table::new(rows, widths)
            .header(header)
            .column_spacing(2)
            .block(
                Block::bordered()
                    .border_style(border)
                    .title(Line::from(" Projects ").style(Style::new().add_modifier(Modifier::BOLD))),
            )
            .row_highlight_style(Style::new().bg(self.color("selected")).add_modifier(Modifier::BOLD))
            .highlight_symbol("▌");
        f.render_stateful_widget(table, area, &mut self.table);
        if self.view.len() > self.page {
            let mut sb = ScrollbarState::new(self.view.len()).position(self.table.selected().unwrap_or(0));
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight).begin_symbol(None).end_symbol(None),
                area.inner(ratatui::layout::Margin { vertical: 1, horizontal: 0 }),
                &mut sb,
            );
        }
        if self.view.is_empty() {
            let msg = Paragraph::new("No projects match. F clears filters, a shows all.")
                .style(dim)
                .centered();
            let [_, mid, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
            f.render_widget(msg, mid);
        }
    }

    fn detail_lines(&self, p: &Project) -> Vec<Line<'static>> {
        let label = |k: &str| Span::styled(format!("{k:<14}"), self.fg("accent"));
        let dim = self.fg("dim");
        let mut out = vec![
            Line::from(Span::styled(p.title.clone(), Style::new().add_modifier(Modifier::BOLD))),
            Line::from(vec![
                Span::styled(format!("{}  {}", p.id, p.slug), dim),
            ]),
            Line::default(),
            Line::from(vec![
                label("status"),
                Span::styled(p.status.to_string(), self.fg(&format!("status.{}", p.status))),
            ]),
            Line::from(vec![label("kind"), Span::raw(p.kind.to_string())]),
            Line::from(vec![
                label("priority"),
                Span::styled(
                    p.priority.map(|x| x.to_string()).unwrap_or_else(|| "unset".into()),
                    self.fg(&format!("priority.{}", p.priority.unwrap_or(Priority::Normal))),
                ),
            ]),
        ];
        let mut field = |k: &str, v: Option<String>| {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                out.push(Line::from(vec![label(k), Span::raw(v)]));
            }
        };
        field("code", p.code.clone());
        field("venue", p.venue.clone());
        field("next", Some(p.next_action.clone().unwrap_or_else(|| "(none recorded)".into())));
        field("path", p.path.as_ref().map(|s| self.reg.abs_path(s).display().to_string()));
        field("repo", p.repo.clone());
        field("collaborators", Some(p.collaborators.join(", ")));
        field("tags", Some(p.tags.join(", ")));
        field("updated", Some(format!("{}  (created {})", p.updated, p.created)));
        if !p.deadlines.is_empty() {
            out.push(Line::default());
            out.push(Line::from(Span::styled("Deadlines", self.fg("accent").add_modifier(Modifier::BOLD))));
            for d in &p.deadlines {
                let days = (d.date - today()).num_days();
                let (when, key) = match days {
                    n if n < 0 => (format!("overdue {}d", -n), "due.overdue"),
                    0 => ("today".into(), "due.overdue"),
                    n if n <= self.cfg.soon_days => (format!("in {n}d"), "due.soon"),
                    n => (format!("in {n}d"), "due.later"),
                };
                out.push(Line::from(vec![
                    Span::raw(format!("  {}  ", d.date)),
                    Span::styled(format!("{when:<12}"), self.fg(key)),
                    Span::raw(d.what.clone()),
                ]));
            }
        }
        if !p.submissions.is_empty() {
            out.push(Line::default());
            out.push(Line::from(Span::styled("Submissions", self.fg("accent").add_modifier(Modifier::BOLD))));
            for s in &p.submissions {
                let note = s.note.as_deref().map(|n| format!("  {n}")).unwrap_or_default();
                out.push(Line::from(format!("  {}  {}  {}{note}", s.date, s.venue, s.outcome)));
            }
        }
        let slug_of = |id: &str| {
            self.all.iter().find(|o| o.id == id).map(|o| o.slug.clone()).unwrap_or_else(|| "(unknown)".into())
        };
        let incoming: Vec<(&Project, &crate::model::Link)> = self
            .all
            .iter()
            .flat_map(|o| o.links.iter().map(move |l| (o, l)))
            .filter(|(_, l)| l.target == p.id)
            .collect();
        if !p.links.is_empty() || !incoming.is_empty() {
            out.push(Line::default());
            out.push(Line::from(Span::styled("Links", self.fg("accent").add_modifier(Modifier::BOLD))));
            for l in &p.links {
                out.push(Line::from(format!("  {} → {} {}", l.rel, l.target, slug_of(&l.target))));
            }
            for (o, l) in incoming {
                out.push(Line::from(format!("  ← {} {} ({})", o.id, o.slug, l.rel)));
            }
        }
        if let Some(n) = &p.notes {
            out.push(Line::default());
            out.push(Line::from(Span::styled("Notes", self.fg("accent").add_modifier(Modifier::BOLD))));
            out.extend(n.lines().map(|l| Line::from(format!("  {l}"))));
        }
        out
    }

    fn draw_detail(&mut self, f: &mut Frame, area: Rect, focused: bool) {
        let lines = self.current().map(|i| self.detail_lines(&self.all[i])).unwrap_or_default();
        let max = lines.len().saturating_sub(1) as u16;
        self.detail_scroll = self.detail_scroll.min(max);
        let border = if focused { self.fg("accent") } else { self.fg("border") };
        let p = Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0))
            .block(
                Block::bordered()
                    .border_style(border)
                    .title(Line::from(" Detail ").style(Style::new().add_modifier(Modifier::BOLD))),
            );
        f.render_widget(p, area);
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let key = |k: &str| Span::styled(k.to_string(), self.fg("accent").add_modifier(Modifier::BOLD));
        let line = match &self.mode {
            Mode::Search => Line::from(vec![key(" / "), Span::raw(self.search.clone()), Span::raw("█"),
                Span::styled("   Enter keep · Esc clear", self.fg("dim"))]),
            Mode::Input { what, buf } => {
                let prompt = match what {
                    InputKind::Next => " next action: ",
                    InputKind::Deadline => " deadline (YYYY-MM-DD what): ",
                    InputKind::Field => " set FIELD=VALUE: ",
                    InputKind::NewTitle(..) => " new project title: ",
                };
                Line::from(vec![key(prompt), Span::raw(buf.clone()), Span::raw("█"),
                    Span::styled("   Enter save · Esc cancel · Ctrl-u clear", self.fg("dim"))])
            }
            _ => match &self.msg {
                Some((m, err)) => Line::from(Span::styled(
                    format!(" {m}"),
                    if *err { self.fg("due.overdue") } else { Style::new().fg(Color::Green) },
                )),
                None => {
                    let mut spans = vec![];
                    for (k, what) in [
                        ("j/k", "move"), ("Enter", "detail"), ("Tab", "pane"), ("/", "search"),
                        ("f", "filter"), ("t", "tag view"), ("s", "sort"), ("c", "columns"), ("S/P/n/D/e", "edit"), ("N", "new"),
                        ("w", "save view"), ("?", "help"), ("q", "quit"),
                    ] {
                        spans.push(key(&format!(" {k}")));
                        spans.push(Span::styled(format!(" {what} "), self.fg("dim")));
                    }
                    Line::from(spans)
                }
            },
        };
        f.render_widget(line, area);
    }

    fn popup(area: Rect, w: u16, h: u16) -> Rect {
        let [r] = Layout::horizontal([Constraint::Length(w.min(area.width))]).flex(Flex::Center).areas(area);
        let [r] = Layout::vertical([Constraint::Length(h.min(area.height))]).flex(Flex::Center).areas(r);
        r
    }

    fn draw_pick(&mut self, f: &mut Frame) {
        let accent = self.color("accent");
        let (title, hint) = match &self.mode {
            Mode::Pick { what, .. } => match what {
                PickKind::FilterField => (" Filter by ".to_string(), " Enter choose · Esc cancel "),
                PickKind::FilterValue(fld) => (format!(" {fld} "), " Enter choose · Esc cancel "),
                PickKind::Status => (" Set status ".into(), " Enter save · Esc cancel "),
                PickKind::Priority => (" Set priority ".into(), " Enter save · Esc cancel "),
                PickKind::Kind => (" Set kind ".into(), " Enter save · Esc cancel "),
                PickKind::NewKind => (" New project: kind ".into(), " Enter choose · Esc cancel "),
                PickKind::Columns => (" Columns ".into(), " Space toggle · J/K reorder · Esc done "),
            },
            _ => return,
        };
        let columns: Vec<String> = self.cfg.columns.clone();
        let Mode::Pick { what, items, state } = &mut self.mode else { return };
        let list_items: Vec<ListItem> = items
            .iter()
            .map(|it| match what {
                PickKind::Columns => {
                    let on = columns.contains(it);
                    ListItem::new(format!("{} {it}", if on { "[x]" } else { "[ ]" }))
                }
                _ => ListItem::new(it.clone()),
            })
            .collect();
        let w = items.iter().map(|s| s.chars().count()).max().unwrap_or(10).max(hint.len()) as u16 + 8;
        let h = items.len() as u16 + 2;
        let area = Self::popup(f.area(), w, h);
        f.render_widget(Clear, area);
        let list = List::new(list_items)
            .block(
                Block::bordered()
                    .border_style(Style::new().fg(accent))
                    .title(Line::from(title).style(Style::new().add_modifier(Modifier::BOLD)))
                    .title_bottom(Line::from(hint).style(Style::new().fg(Color::DarkGray))),
            )
            .highlight_style(Style::new().fg(Color::Black).bg(accent))
            .highlight_symbol("› ");
        f.render_stateful_widget(list, area, state);
    }

    fn draw_help(&self, f: &mut Frame) {
        let rows = [
            ("Navigate", ""),
            ("j/k ↑/↓", "move (scrolls the detail pane when it has focus)"),
            ("PgUp/PgDn  Ctrl-u/d", "page / half page"),
            ("g/G  Home/End", "first / last"),
            ("mouse", "wheel scrolls, click selects"),
            ("Enter", "full-screen detail on/off"),
            ("Tab", "focus list / detail pane"),
            ("v", "detail pane on/off"),
            ("< >", "widen / narrow detail pane"),
            ("View", ""),
            ("/", "live search (id, code, slug, title, next, venue, tags, notes)"),
            ("f  F", "filter by status/kind/priority/tag/code  /  clear filters"),
            ("t", "cycle quick tag views (quick_tags in the config)"),
            ("a", "show all (incl. published, archived, hidden)"),
            ("s  r", "cycle sort  /  reverse"),
            ("c", "choose and reorder columns"),
            ("w", "save this view as the default"),
            ("Edit selected project", ""),
            ("S  P  K", "status / priority / kind"),
            ("n", "next action"),
            ("D", "add deadline"),
            ("e", "any field: FIELD=VALUE (venue, tags, notes, ...)"),
            ("N", "new project: kind (or kind [group]), then title; takes the current filters"),
            ("R", "reload from disk"),
            ("q", "quit"),
        ];
        let lines: Vec<Line> = rows
            .iter()
            .map(|(k, v)| {
                if v.is_empty() {
                    Line::from(Span::styled(*k, self.fg("accent").add_modifier(Modifier::BOLD)))
                } else {
                    Line::from(vec![
                        Span::styled(format!("  {k:<22}"), Style::new().add_modifier(Modifier::BOLD)),
                        Span::raw(*v),
                    ])
                }
            })
            .collect();
        let path = self.cfg_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        let area = Self::popup(f.area(), 84, lines.len() as u16 + 2);
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_style(self.fg("accent"))
                    .title(Line::from(" Keys ").style(Style::new().add_modifier(Modifier::BOLD)))
                    .title_bottom(Line::from(format!(" config: {path} ")).style(self.fg("dim"))),
            ),
            area,
        );
    }
}

/// `base`, or `base-2`, `base-3`, ... when the slug is taken.
fn unique_slug(all: &[Project], base: &str) -> String {
    let taken = |s: &str| all.iter().any(|p| p.slug == s);
    if !taken(base) {
        return base.to_string();
    }
    (2..).map(|n| format!("{base}-{n}")).find(|s| !taken(s)).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips() {
        let mut c = Config::default();
        c.kind = Some(Kind::Paper);
        c.colors.insert("accent".into(), "#89b4fa".into());
        let back: Config = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(back.kind, Some(Kind::Paper));
        assert_eq!(back.columns, c.columns);
        assert_eq!(back.colors["accent"], "#89b4fa");
    }

    #[test]
    fn unique_slug_appends_a_counter() {
        let mut p: Project = toml::from_str(
            "id = \"p0001\"\nslug = \"aurora\"\ntitle = \"Aurora\"\nkind = \"paper\"\nstatus = \"active\"\ncreated = \"2026-01-01\"\nupdated = \"2026-01-01\"\n",
        )
        .unwrap();
        assert_eq!(unique_slug(&[p.clone()], "kelp"), "kelp");
        assert_eq!(unique_slug(&[p.clone()], "aurora"), "aurora-2");
        let q = p.clone();
        p.slug = "aurora-2".into();
        assert_eq!(unique_slug(&[q, p], "aurora"), "aurora-3");
    }

    #[test]
    fn default_color_keys_parse_from_config() {
        let c: Config = toml::from_str("[colors]\n\"status.active\" = \"light-green\"\n").unwrap();
        assert_eq!(c.colors["status.active"].parse::<Color>().unwrap(), Color::LightGreen);
        assert!(default_colors().contains_key("status.active"));
    }
}
