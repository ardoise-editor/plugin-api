//! What the API plugin holds: the collections, environments and history,
//! the requests open in tabs with their answers, and the window open over
//! the panel.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::api::checks::{self, Outcome};
use crate::api::http::{self, Response};
use crate::api::model::{self, Collection, Environment, Folder, Item, Request};
use crate::api::store::{self, HistoryEntry};

/// Most of a body that is shown.
pub const MAX_SHOWN: usize = 2 * 1024 * 1024;
/// The size up to which a body is parsed to be laid out.
const MAX_PRETTY: usize = 8 * MAX_SHOWN;
/// The longest line shown: a longer one (a minified body) is broken.
const MAX_LINE: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ReqSection {
    #[default]
    Params,
    Headers,
    Body,
    Auth,
    Settings,
    Tests,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RespSection {
    #[default]
    Body,
    Headers,
    Cookies,
    Tests,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Side {
    #[default]
    Collections,
    History,
}

/// The window over the panel.
#[derive(Debug, Clone, PartialEq)]
pub enum Dialog {
    /// A name for a new collection, folder or environment, or to rename.
    Name {
        what: NameFor,
        text: String,
    },
    Delete(Removal),
    /// A `curl` command to read.
    Curl {
        error: Option<String>,
    },
    Environments,
    /// What an import or a run said.
    /// What an import or a run of a collection found: title, lines.
    Report(String, Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameFor {
    NewCollection,
    /// A folder in a collection (0: at the top).
    NewFolder(u64, u64),
    /// A collection (item 0), a folder or a request.
    Rename(u64, u64),
    NewEnvironment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// A collection (item 0), a folder or a request.
    Item(u64, u64),
    Environment(u64),
}

/// An answer, ready to be read.
pub struct ResponseView {
    pub response: Response,
    pub kind: http::Kind,
    pub pretty: Option<String>,
    pub show_pretty: bool,
    /// The text shown (laid out or as it came, cut and broken).
    pub shown: String,
    pub cut: bool,
    pub search: String,
    /// The match Enter went to in the search: its start and end in `shown`.
    pub found: Option<(usize, usize)>,
    pub outcomes: Vec<Outcome>,
}

/// The place of the next `needle` in `text`, from `from` on and then from
/// the start (whatever the case, for the letters of ASCII): its start and
/// end.
pub fn find_next(text: &str, needle: &str, from: usize) -> Option<(usize, usize)> {
    if needle.is_empty() {
        return None;
    }
    let (haystack, needle) = (text.to_ascii_lowercase(), needle.to_ascii_lowercase());
    let mut from = from.min(haystack.len());
    while !haystack.is_char_boundary(from) {
        from -= 1;
    }
    let start = haystack[from..]
        .find(&needle)
        .map(|i| i + from)
        .or_else(|| haystack.find(&needle))?;
    Some((start, start + needle.len()))
}

/// The line and the column (in characters) of a place of the text.
pub fn position_of(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count();
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    (line, before[start..].chars().count())
}

/// The place in the text of a line and a column (in characters).
pub fn offset_of(text: &str, line: usize, column: usize) -> usize {
    let mut offset = 0;
    for (n, l) in text.split('\n').enumerate() {
        if n == line {
            return offset + l.char_indices().nth(column).map_or(l.len(), |(i, _)| i);
        }
        offset += l.len() + 1;
    }
    text.len()
}

/// The text with its lines longer than `MAX_LINE` broken.
pub fn break_long_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / MAX_LINE + 1);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let mut rest = line;
        while rest.len() > MAX_LINE {
            let mut at = MAX_LINE;
            while !rest.is_char_boundary(at) {
                at -= 1;
            }
            out.push_str(&rest[..at]);
            out.push('\n');
            rest = &rest[at..];
        }
        out.push_str(rest);
    }
    out
}

/// An answer with what costs time worked out (off the main thread).
pub struct Processed {
    pub response: Response,
    kind: http::Kind,
    pretty: Option<String>,
    pub outcomes: Vec<Outcome>,
    /// The values to keep: the variable, and the value or why there is none.
    pub kept: Vec<(String, Result<String, String>)>,
}

/// Parses the body once for everything: its kind, its layout, the tests
/// and the values kept.
pub fn process(
    response: Response,
    tests: &[model::Check],
    extracts: &[model::Extract],
    vars: &HashMap<String, String>,
) -> Processed {
    let kind = http::kind_of(response.header("content-type"), &response.body);
    let pretty = if kind == http::Kind::Binary || response.body.len() > MAX_PRETTY {
        None
    } else {
        http::pretty(kind, &response.text())
    };
    let json = if tests.iter().any(|c| c.enabled) || extracts.iter().any(|e| e.enabled) {
        checks::parse(&response)
    } else {
        None
    };
    let outcomes = checks::evaluate_with(tests, &response, vars, &json);
    let kept = checks::extract_with(extracts, &response, &json);
    Processed {
        response,
        kind,
        pretty,
        outcomes,
        kept,
    }
}

impl ResponseView {
    pub fn new(done: Processed) -> ResponseView {
        let mut view = ResponseView {
            response: done.response,
            kind: done.kind,
            show_pretty: done.pretty.is_some(),
            pretty: done.pretty,
            shown: String::new(),
            cut: false,
            search: String::new(),
            found: None,
            outcomes: done.outcomes,
        };
        view.show();
        view
    }

    /// Puts the body, laid out or as it came, in the viewer.
    pub fn show(&mut self) {
        let raw;
        let shown: &str = match (&self.pretty, self.show_pretty) {
            (Some(pretty), true) => pretty,
            _ if self.kind == http::Kind::Binary => "",
            _ => {
                let end = self.response.body.len().min(MAX_SHOWN + 4);
                raw = String::from_utf8_lossy(&self.response.body[..end]).into_owned();
                &raw
            }
        };
        self.cut = shown.len() > MAX_SHOWN;
        let mut end = shown.len().min(MAX_SHOWN);
        while !shown.is_char_boundary(end) {
            end -= 1;
        }
        self.shown = break_long_lines(&shown[..end]);
        self.found = None;
    }

    /// How many times what is searched is in the body.
    pub fn matches(&self) -> usize {
        let needle = self.search.to_ascii_lowercase();
        if needle.is_empty() {
            return 0;
        }
        self.shown.to_ascii_lowercase().matches(&needle).count()
    }

    /// The extension the body is highlighted as.
    pub fn language(&self) -> &'static str {
        match (self.kind, self.show_pretty) {
            (http::Kind::Json, _) => "json",
            (http::Kind::Xml, _) => "xml",
            (http::Kind::Html, _) => "html",
            _ => "",
        }
    }
}

/// A request open in a tab.
#[derive(Serialize, Deserialize)]
pub struct ReqTab {
    pub request: Request,
    /// The collection and the request it came from, once it is kept.
    pub origin: Option<(u64, u64)>,
    /// The request as it was last kept, to know what changed.
    pub kept: Request,
    #[serde(default)]
    pub section: ReqSection,
    /// The collection a new request is kept in.
    #[serde(default)]
    pub save_target: Option<u64>,
    #[serde(skip)]
    pub response: Option<ResponseView>,
    #[serde(skip)]
    pub resp_section: RespSection,
    #[serde(skip)]
    pub sending: bool,
    /// The number of the sending that counts: an older answer is dropped.
    #[serde(skip)]
    pub token: u64,
    #[serde(skip)]
    pub error: Option<String>,
    #[serde(skip)]
    pub notice: Option<String>,
}

impl ReqTab {
    pub fn new(request: Request, origin: Option<(u64, u64)>) -> ReqTab {
        ReqTab {
            kept: request.clone(),
            request,
            origin,
            section: ReqSection::Params,
            save_target: None,
            response: None,
            resp_section: RespSection::Body,
            sending: false,
            token: 0,
            error: None,
            notice: None,
        }
    }

    /// Something changed since the request was kept.
    pub fn changed(&self) -> bool {
        self.request != self.kept
    }

    pub fn title(&self) -> String {
        let t = self.request.title();
        if t.is_empty() {
            t!("api.untitled").to_string()
        } else {
            t
        }
    }
}

/// The folder of an id in a collection.
pub fn folder_of(collection: &Collection, id: u64) -> Option<&Folder> {
    fn find(items: &[Item], id: u64) -> Option<&Folder> {
        for item in items {
            if let Item::Folder(f) = item {
                if f.id == id {
                    return Some(f);
                }
                if let Some(found) = find(&f.items, id) {
                    return Some(found);
                }
            }
        }
        None
    }
    find(&collection.items, id)
}

/// What the plugin holds.
pub struct Api {
    pub collections: Vec<Collection>,
    pub environments: Vec<Environment>,
    pub active_env: Option<u64>,
    pub history: Vec<HistoryEntry>,
    pub set_aside: Vec<String>,
    pub next_id: u64,
    /// The collections and folders open in the panel.
    pub open: HashSet<u64>,
    pub filter: String,
    pub side: Side,
    /// The item picked in the tree: (collection, item; 0 for the collection).
    pub picked: Option<(u64, u64)>,
    pub dialog: Option<Dialog>,
    /// The environment being edited in its window.
    pub editing_env: Option<u64>,
    /// The `curl` command typed in its window.
    pub curl: String,
    /// The requests open in tabs, by tab.
    pub tabs: BTreeMap<String, ReqTab>,
    /// How many tabs were opened (they are named after it).
    pub counter: u64,
    /// A collection is being run.
    pub running: Option<String>,
}

impl Api {
    /// Nothing yet (before the start).
    pub fn empty() -> Api {
        Api {
            collections: Vec::new(),
            environments: Vec::new(),
            active_env: None,
            history: Vec::new(),
            set_aside: Vec::new(),
            next_id: 0,
            open: HashSet::new(),
            filter: String::new(),
            side: Side::Collections,
            picked: None,
            dialog: None,
            editing_env: None,
            curl: String::new(),
            tabs: BTreeMap::new(),
            counter: 0,
            running: None,
        }
    }

    pub fn load() -> Api {
        let stored = store::load();
        let mut api = Api {
            collections: stored.collections,
            environments: stored.environments,
            active_env: stored.active_env,
            history: stored.history,
            set_aside: stored.set_aside,
            next_id: stored.next_id,
            open: HashSet::new(),
            filter: String::new(),
            side: Side::Collections,
            picked: None,
            dialog: None,
            editing_env: None,
            curl: String::new(),
            tabs: BTreeMap::new(),
            counter: 0,
            running: None,
        };
        // Whatever has no number gets one (a file written by hand).
        let mut next = api.next_id;
        for collection in &mut api.collections {
            if collection.id == 0 {
                next += 1;
                collection.id = next;
            }
            collection.number(&mut next);
        }
        api.next_id = next;
        if api
            .active_env
            .is_some_and(|id| !api.environments.iter().any(|e| e.id == id))
        {
            api.active_env = None;
        }
        api
    }

    pub fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Puts a value in a variable of the environment that is on; `false`
    /// when none is.
    pub fn set_variable(&mut self, name: &str, value: &str) -> bool {
        let Some(id) = self.active_env else {
            return false;
        };
        let Some(env) = self.environments.iter_mut().find(|e| e.id == id) else {
            return false;
        };
        match env.vars.iter_mut().find(|v| v.key.trim() == name) {
            Some(var) => {
                var.value = value.to_string();
                var.enabled = true;
            }
            None => env.vars.push(model::Variable {
                enabled: true,
                key: name.to_string(),
                value: value.to_string(),
                secret: false,
            }),
        }
        true
    }

    pub fn collection(&self, id: u64) -> Option<&Collection> {
        self.collections.iter().find(|c| c.id == id)
    }

    pub fn collection_mut(&mut self, id: u64) -> Option<&mut Collection> {
        self.collections.iter_mut().find(|c| c.id == id)
    }

    pub fn environment(&self, id: u64) -> Option<&Environment> {
        self.environments.iter().find(|e| e.id == id)
    }

    pub fn environment_mut(&mut self, id: u64) -> Option<&mut Environment> {
        self.environments.iter_mut().find(|e| e.id == id)
    }

    /// The variables of the environment that is on.
    pub fn variables(&self) -> HashMap<String, String> {
        self.active_env
            .and_then(|id| self.environment(id))
            .map(Environment::values)
            .unwrap_or_default()
    }

    pub fn save_collection(&self, id: u64) {
        if let Some(c) = self.collection(id) {
            store::save_collection(c);
        }
    }

    pub fn save_environments(&self) {
        store::save_environments(&self.environments, self.active_env, self.next_id);
    }

    /// Done when it returns, for the editor that closes.
    pub fn save_environments_now(&self) {
        store::save_environments_now(&self.environments, self.active_env, self.next_id);
    }

    /// Adds a request that was sent to the history, newest first.
    pub fn remember(&mut self, entry: HistoryEntry) {
        self.history.insert(0, entry);
        self.history.truncate(store::MAX_HISTORY);
        store::save_history(&self.history);
    }

    /// A folder is open in the panel (the filter opens them all).
    pub fn is_open(&self, id: u64) -> bool {
        !self.filter.trim().is_empty() || self.open.contains(&id)
    }

    /// The tab of a request of a collection, if one is open.
    pub fn tab_of(&self, collection: u64, request: u64) -> Option<String> {
        self.tabs
            .iter()
            .find(|(_, t)| t.origin == Some((collection, request)))
            .map(|(id, _)| id.clone())
    }

    /// A new tab for a request; its id.
    pub fn add_tab(&mut self, request: Request, origin: Option<(u64, u64)>) -> String {
        self.counter += 1;
        let mut id = format!("req-{}", self.counter);
        while self.tabs.contains_key(&id) {
            self.counter += 1;
            id = format!("req-{}", self.counter);
        }
        self.tabs.insert(id.clone(), ReqTab::new(request, origin));
        id
    }

    /// The tabs whose request no longer is in its collection come loose.
    pub fn release_orphans(&mut self) {
        let alive: Vec<(u64, u64)> = self
            .tabs
            .values()
            .filter_map(|t| t.origin)
            .filter(|(c, r)| {
                self.collection(*c)
                    .is_some_and(|coll| coll.request(*r).is_some())
            })
            .collect();
        for tab in self.tabs.values_mut() {
            if tab.origin.is_some_and(|o| !alive.contains(&o)) {
                tab.origin = None;
            }
        }
    }
}

/// The current time, as the history writes it: `YYYY-MM-DD HH:MM:SS`.
pub fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_in_the_search_goes_to_the_next_match_and_comes_back_round() {
        let text = "Alpha beta\nalpha é alpha";
        let first = find_next(text, "ALPHA", 0).unwrap();
        assert_eq!(first, (0, 5));
        let second = find_next(text, "alpha", first.1).unwrap();
        assert_eq!(position_of(text, second.0), (1, 0));
        let third = find_next(text, "alpha", second.1).unwrap();
        // "é" is one character, two bytes.
        assert_eq!(position_of(text, third.0), (1, 8));
        assert_eq!(offset_of(text, 1, 8), third.0);
        assert_eq!(
            find_next(text, "alpha", third.1),
            Some(first),
            "round again"
        );
        assert_eq!(find_next(text, "zeta", 0), None);
        assert_eq!(find_next(text, "", 0), None);
        assert_eq!(find_next(text, "a", 99).map(|f| f.0), Some(0));
    }

    #[test]
    fn a_very_long_line_is_broken_for_showing() {
        let text = format!("{}\nshort\n{}", "é".repeat(MAX_LINE), "a".repeat(10));
        let shown = break_long_lines(&text);
        assert!(shown.lines().all(|l| l.len() <= MAX_LINE));
        assert_eq!(shown.replace('\n', ""), text.replace('\n', ""));
    }

    #[test]
    fn the_time_is_written_as_the_history_wants_it() {
        let now = now();
        assert_eq!(now.len(), 19);
        assert_eq!(&now[4..5], "-");
        assert!(now.starts_with("20"));
    }
}
