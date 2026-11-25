//! The launcher's data: the TV's apps as rows, in tabs and sections.

use super::*;

/// A row in the launcher.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub alias: Option<&'static str>,
    /// What to pass to `apps::start`.
    pub target: String,
    pub package: String,
    /// What kind of app it is, shown beside apps without an alias ("player").
    pub tag: Option<&'static str>,
    pub tab: AppTab,
    /// The heading it's listed under in the Other tab.
    pub section: &'static str,
}

/// A package's friendly name, for the status line.
pub fn app_name(pkg: &str) -> String {
    apps::by_package(pkg).map_or_else(|| crate::labels::name(pkg).unwrap_or_else(|| pkg.to_string()), |a| list_name(a.name))
}

/// Other-tab headings, in the order they're shown.
pub const SECTIONS: &[&str] = &["Other home apps", "Video & music", "Players", "Live TV", "Stores", "Other apps"];

pub(crate) fn section_of(kind: apps::Kind) -> &'static str {
    match kind {
        apps::Kind::Launcher => SECTIONS[0],
        apps::Kind::Video | apps::Kind::Music => SECTIONS[1],
        apps::Kind::Player => SECTIONS[2],
        apps::Kind::LiveTv => SECTIONS[3],
        apps::Kind::Store => SECTIONS[4],
        apps::Kind::Other => SECTIONS[5],
    }
}

/// The launcher's rows. Apps tab: the aliased apps, `home` standing for the
/// default home app. Other tab: home apps that aren't the default, then the
/// rest of the TV-launcher apps, by section.
pub fn launcher_entries(known: &[&'static apps::App], guessed: &[apps::Guess], default_home: Option<&str>) -> Vec<Entry> {
    let is_default = |pkg: &str| default_home == Some(pkg);
    let mut rows: Vec<Entry> = Vec::new();
    let mut others: Vec<Entry> = Vec::new();
    for a in known {
        let pkg = a.package();
        let other_home = a.kind == apps::Kind::Launcher && a.target != "@home";
        if other_home && is_default(pkg) {
            // that's what "home" opens
            continue;
        }
        let e = Entry {
            // "the home screen" says nothing about which one; the package does
            name: if other_home { crate::labels::name(pkg).unwrap_or_else(|| pkg.to_string()) } else { list_name(a.name) },
            alias: a.alias,
            target: a.alias.unwrap_or(a.target).to_string(),
            package: pkg.to_string(),
            tag: None,
            tab: if other_home { AppTab::Other } else { AppTab::Known },
            section: section_of(a.kind),
        };
        if other_home { others.push(e) } else { rows.push(e) }
    }
    for g in guessed {
        if is_default(&g.package) || rows.iter().chain(&others).any(|r| r.package == g.package) {
            continue;
        }
        let Some(component) = g.component.clone() else { continue };
        let table = apps::by_package(&g.package);
        // generic table names ("the home screen", "a NetTV app") don't say
        // which app this is; the package does
        // which app this is; its own name does (or failing that, its package)
        let name = match table {
            Some(a) if !a.name.starts_with("a ") && !a.name.starts_with("the ") => list_name(a.name),
            _ => crate::labels::name(&g.package).unwrap_or_else(|| g.package.clone()),
        };
        let kind = table.map_or(g.kind, |a| a.kind);
        let tag = (kind != apps::Kind::Other).then(|| kind.as_str());
        others.push(Entry { name, alias: None, target: component, package: g.package.clone(), tag, tab: AppTab::Other, section: section_of(kind) });
    }
    let order = |e: &Entry| SECTIONS.iter().position(|s| *s == e.section).unwrap_or(SECTIONS.len());
    others.sort_by_key(|e| (order(e), e.name.to_lowercase()));
    rows.extend(others);
    rows
}

/// The launcher's tabs: apps tveee has aliases for, and the rest of the
/// TV-launcher apps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppTab {
    Known,
    Other,
}

impl AppTab {
    pub fn other(self) -> AppTab {
        if self == AppTab::Known { AppTab::Other } else { AppTab::Known }
    }

    pub fn holds(self, e: &Entry) -> bool {
        e.tab == self
    }
}

