//! The API plugin of Ardoise: a light version of Postman. Requests to an
//! API in tabs, the collections that keep them, the environments with their
//! variables, the history, tests of the answers, imports and exports of
//! Postman, OpenAPI and cURL.

#[macro_use]
extern crate rust_i18n;

i18n!("locales", fallback = "en");

mod api;
mod fsutil;
mod secrets;
mod state;
mod ui;

use std::sync::{Arc, Mutex, MutexGuard};

use ardoise_plugin::protocol::{Context, InitializeParams, Settings as EditorSettings, UiEvent};
use ardoise_plugin::ui::{column, input, text, Node, TextStyle};
use ardoise_plugin::{run, Host, Plugin};
use serde::{Deserialize, Serialize};

use api::model::{
    self, CheckOp, CheckSource, Collection, Environment, ExtractSource, Folder, Item, Method, Pair,
    Request, Variable,
};
use api::store::HistoryEntry;
use api::{checks, curl, http, openapi, postman, store};
use state::{Api, Dialog, NameFor, Removal, ReqSection, RespSection, ResponseView, Side};

const PANEL: &str = "api";
const SETTINGS: &str = "settings";

/// What the client trusts and goes through (Preferences › Plugins).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct NetworkSettings {
    #[serde(default)]
    ca_file: String,
    #[serde(default)]
    client_cert: String,
    #[serde(default)]
    proxy: String,
}

impl NetworkSettings {
    fn apply(&self) {
        http::set_network(http::Network {
            ca_file: self.ca_file.clone(),
            client_cert: self.client_cert.clone(),
            proxy: self.proxy.clone(),
        });
    }
}

/// The requests open in tabs, kept between two runs of the editor.
#[derive(Default, Serialize, Deserialize)]
struct KeptTabs {
    #[serde(default)]
    tabs: std::collections::BTreeMap<String, state::ReqTab>,
    #[serde(default)]
    counter: u64,
}

struct ApiPlugin {
    shared: Arc<Mutex<Api>>,
    network: NetworkSettings,
    data_dir: std::path::PathBuf,
    runtime: tokio::runtime::Runtime,
    /// Removed with its data: nothing is written again, the keychain least
    /// of all.
    forgotten: bool,
}

fn lock(shared: &Arc<Mutex<Api>>) -> MutexGuard<'_, Api> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// The table of a request a name of the panel speaks of.
/// The collection and the id (0: the collection itself) of a row of the
/// tree: `c:<collection>:0`, `f:<collection>:<folder>`, `r:<collection>:<request>`.
fn tree_row(item: &str) -> Option<(u64, u64)> {
    let parts: Vec<u64> = item
        .split(':')
        .skip(1)
        .filter_map(|n| n.parse().ok())
        .collect();
    Some((*parts.first()?, *parts.get(1)?))
}

fn pairs_mut<'a>(request: &'a mut Request, table: &str) -> Option<&'a mut Vec<Pair>> {
    match table {
        "params" => Some(&mut request.params),
        "headers" => Some(&mut request.headers),
        "form" => Some(&mut request.body.form),
        _ => None,
    }
}

impl ApiPlugin {
    fn api(&self) -> MutexGuard<'_, Api> {
        lock(&self.shared)
    }

    fn draw_panel(&self, host: &Host) {
        let api = self.api();
        host.render(PANEL, ui::panel(&api));
    }

    fn draw_tab(&self, host: &Host, id: &str) {
        let api = self.api();
        if let Some(tab) = api.tabs.get(id) {
            host.render(id, ui::tab(&api, tab));
            host.tab_dirty(id, tab.changed());
        }
    }

    fn tabs_file(&self) -> std::path::PathBuf {
        self.data_dir.join("tabs.json")
    }

    /// Keeps the requests of the tabs (without their credentials).
    fn keep_tabs(&self) {
        let api = self.api();
        let bare = |request: &Request| {
            let mut request = request.clone();
            for value in [
                &mut request.auth.token,
                &mut request.auth.password,
                &mut request.auth.key_value,
            ] {
                if !value.contains("{{") {
                    value.clear();
                }
            }
            request
        };
        let mut kept = KeptTabs {
            counter: api.counter,
            ..KeptTabs::default()
        };
        for (id, tab) in &api.tabs {
            let mut copy = state::ReqTab::new(bare(&tab.request), tab.origin);
            copy.kept = bare(&tab.kept);
            copy.section = tab.section;
            copy.save_target = tab.save_target;
            kept.tabs.insert(id.clone(), copy);
        }
        if let Ok(json) = serde_json::to_string_pretty(&kept) {
            let _ = fsutil::write_atomic(&self.tabs_file(), json.as_bytes(), true);
        }
    }

    /// Opens a request in a tab of its own (the tab it has, if it has one).
    fn open(&self, host: &Host, request: Request, origin: Option<(u64, u64)>) {
        let (id, title) = {
            let mut api = self.api();
            if let Some(id) = origin.and_then(|(c, r)| api.tab_of(c, r)) {
                let title = api.tabs[&id].title();
                (id, title)
            } else {
                let id = api.add_tab(request, origin);
                let title = api.tabs[&id].title();
                (id, title)
            }
        };
        let _ = host.open_tab(&id, title);
        self.draw_tab(host, &id);
        self.keep_tabs();
    }

    /// Keeps the request of a tab in its collection (a new one, in the one
    /// chosen, the first, or a new one).
    fn save(&self, host: &Host, id: &str) {
        let title = {
            let mut api = self.api();
            let Some(tab) = api.tabs.get(id) else {
                return;
            };
            let (request, origin, target) = (tab.request.clone(), tab.origin, tab.save_target);
            match origin {
                Some((c, r)) => {
                    if let Some(slot) = api.collection_mut(c).and_then(|col| col.request_mut(r)) {
                        let mut kept = request;
                        kept.id = r;
                        *slot = kept;
                    }
                    api.save_collection(c);
                }
                None => {
                    let collection = target
                        .filter(|c| api.collection(*c).is_some())
                        .or_else(|| api.collections.first().map(|c| c.id));
                    let collection = match collection {
                        Some(c) => c,
                        None => {
                            let c = api.new_id();
                            api.collections.push(Collection {
                                id: c,
                                name: t!("api.default_collection").to_string(),
                                items: Vec::new(),
                            });
                            c
                        }
                    };
                    let r = api.new_id();
                    let mut kept = request;
                    kept.id = r;
                    if let Some(col) = api.collection_mut(collection) {
                        col.items.push(Item::Request(kept));
                    }
                    api.open.insert(collection);
                    api.save_collection(collection);
                    if let Some(tab) = api.tabs.get_mut(id) {
                        tab.origin = Some((collection, r));
                    }
                }
            }
            let tab = api.tabs.get_mut(id).expect("the tab");
            tab.kept = tab.request.clone();
            tab.title()
        };
        let _ = host.open_tab(id, title);
        self.draw_tab(host, id);
        self.draw_panel(host);
        self.keep_tabs();
    }

    /// Sends the request of a tab; its answer comes back on its own.
    fn send(&self, host: &Host, id: &str) {
        let (resolved, checks, extracts, vars, token) = {
            let mut api = self.api();
            let vars = api.variables();
            let Some(tab) = api.tabs.get_mut(id) else {
                return;
            };
            if tab.sending {
                return;
            }
            tab.token += 1;
            tab.sending = true;
            tab.error = None;
            tab.notice = None;
            (
                tab.request.resolve(&vars),
                tab.request.checks.clone(),
                tab.request.extracts.clone(),
                vars,
                tab.token,
            )
        };
        self.draw_tab(host, id);
        let (shared, host, id) = (self.shared.clone(), host.clone(), id.to_string());
        self.runtime.spawn(async move {
            let result = match http::send(resolved).await {
                Ok(response) => tokio::task::spawn_blocking(move || {
                    state::process(response, &checks, &extracts, &vars)
                })
                .await
                .map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            let mut api = lock(&shared);
            let Some(tab) = api.tabs.get_mut(&id) else {
                return;
            };
            if tab.token != token || !tab.sending {
                return;
            }
            tab.sending = false;
            let request = tab.request.clone();
            let mut kept = Vec::new();
            let entry = match result {
                Ok(mut done) => {
                    let entry = HistoryEntry {
                        time: state::now(),
                        request,
                        status: Some(done.response.status),
                        ms: done.response.ms,
                        error: None,
                    };
                    kept = std::mem::take(&mut done.kept);
                    tab.response = Some(ResponseView::new(done));
                    tab.resp_section = RespSection::Body;
                    entry
                }
                Err(error) => {
                    tab.response = None;
                    tab.error = Some(error.clone());
                    HistoryEntry {
                        time: state::now(),
                        request,
                        status: None,
                        ms: 0,
                        error: Some(error),
                    }
                }
            };
            // The values asked for go to the environment that is on.
            let (mut notes, mut names) = (Vec::new(), Vec::new());
            for (name, found) in kept {
                match found {
                    Ok(value) => {
                        if api.set_variable(&name, &value) {
                            names.push(name);
                        } else {
                            notes.push(t!("api.value_not_kept", name = name.as_str()).to_string());
                        }
                    }
                    Err(e) => notes.push(format!("{name}: {e}")),
                }
            }
            if !names.is_empty() {
                api.save_environments();
                notes.insert(
                    0,
                    t!("api.kept", names = names.join(", ").as_str()).to_string(),
                );
            }
            if !notes.is_empty() {
                if let Some(tab) = api.tabs.get_mut(&id) {
                    tab.notice = Some(notes.join(" · "));
                }
            }
            api.remember(entry);
            if let Some(tab) = api.tabs.get(&id) {
                host.render(&id, ui::tab(&api, tab));
            }
            host.render(PANEL, ui::panel(&api));
        });
    }

    /// Runs every request of a collection, in order, and reports.
    fn run_collection(&self, host: &Host, id: u64) {
        let (requests, name, vars) = {
            let mut api = self.api();
            let Some(collection) = api.collection(id) else {
                return;
            };
            fn flat(items: &[Item], out: &mut Vec<Request>) {
                for item in items {
                    match item {
                        Item::Request(r) => out.push(r.clone()),
                        Item::Folder(f) => flat(&f.items, out),
                    }
                }
            }
            let mut requests = Vec::new();
            flat(&collection.items, &mut requests);
            if requests.is_empty() || api.running.is_some() {
                return;
            }
            let name = collection.name.clone();
            api.running = Some(name.clone());
            (requests, name, api.variables())
        };
        host.notify(t!("api.running", name = name.as_str(), n = requests.len()).to_string());
        let (shared, host) = (self.shared.clone(), host.clone());
        self.runtime.spawn(async move {
            let (results, kept) = checks::run_all(requests, vars).await;
            let mut api = lock(&shared);
            // Stopped meanwhile: what it found is dropped.
            if api.running.as_deref() != Some(name.as_str()) {
                return;
            }
            api.running = None;
            let mut lines = Vec::new();
            let (mut ok, mut bad) = (0, 0);
            for r in &results {
                let good = r.error.is_none() && r.failed.is_empty();
                if good {
                    ok += 1;
                } else {
                    bad += 1;
                }
                let mark = if good { "✓" } else { "✗" };
                let how = match (&r.error, r.status) {
                    (Some(e), _) => e.clone(),
                    (None, Some(s)) => {
                        format!("{s} · {} ms · {}", r.ms, t!("api.run_tests", n = r.passed))
                    }
                    _ => String::new(),
                };
                lines.push(format!("{mark} {} — {how}", r.name));
                for f in &r.failed {
                    lines.push(format!("      ✗ {f}"));
                }
            }
            lines.insert(
                0,
                t!("api.run_summary", name = name.as_str(), ok = ok, bad = bad).to_string(),
            );
            let mut changed = false;
            for (variable, value) in kept {
                changed |= api.set_variable(&variable, &value);
            }
            if changed {
                api.save_environments();
            }
            api.dialog = Some(Dialog::Report(lines));
            host.render(PANEL, ui::panel(&api));
        });
    }

    /// A file of Postman or OpenAPI, picked by the user.
    fn import(&self, host: &Host) {
        let Ok(Some(path)) = host.pick_file(t!("api.import"), &["json", "yaml", "yml"]) else {
            return;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                self.api().dialog = Some(Dialog::Report(vec![format!("{}: {e}", path.display())]));
                return;
            }
        };
        let mut api = self.api();
        if postman::is_environment(&text) {
            match postman::import_environment(&text) {
                Ok(mut env) => {
                    env.id = api.new_id();
                    let name = env.name.clone();
                    api.editing_env = Some(env.id);
                    api.environments.push(env);
                    api.save_environments();
                    host.notify(t!("api.imported_env", name = name.as_str()).to_string());
                }
                Err(e) => api.dialog = Some(Dialog::Report(vec![e])),
            }
            return;
        }
        let read =
            openapi::import_if_openapi(&text).unwrap_or_else(|| postman::import_collection(&text));
        match read {
            Ok(imported) => {
                let mut collection = imported.collection;
                collection.id = api.new_id();
                let mut next = api.next_id;
                collection.number(&mut next);
                api.next_id = next;
                let (id, name, count) =
                    (collection.id, collection.name.clone(), collection.count());
                api.collections.push(collection);
                api.save_collection(id);
                api.open.insert(id);
                let mut lines =
                    vec![t!("api.imported", name = name.as_str(), n = count).to_string()];
                if !imported.variables.is_empty() {
                    let env = Environment {
                        id: api.new_id(),
                        name: name.clone(),
                        vars: imported.variables,
                    };
                    lines.push(t!("api.imported_vars", name = name.as_str()).to_string());
                    api.environments.push(env);
                    api.save_environments();
                }
                lines.extend(imported.warnings);
                if lines.len() > 1 {
                    api.dialog = Some(Dialog::Report(lines));
                } else {
                    host.notify(lines.remove(0));
                }
            }
            Err(e) => api.dialog = Some(Dialog::Report(vec![e])),
        }
    }

    fn export(&self, host: &Host, name: String, text: String) {
        match host.save_file(name, text) {
            Ok(true) => host.notify(
                t!("api.exported", path = "")
                    .to_string()
                    .trim_end_matches(' ')
                    .to_string(),
            ),
            Ok(false) => {}
            Err(e) => host.error(e.message),
        }
    }

    /// The OK of the window over the panel.
    fn dialog_ok(&self, host: &Host) {
        let mut api = self.api();
        let Some(dialog) = api.dialog.clone() else {
            return;
        };
        match dialog {
            Dialog::Name { what, text } => {
                let name = text.trim().to_string();
                if name.is_empty() {
                    return;
                }
                api.dialog = None;
                match what {
                    NameFor::NewCollection => {
                        let id = api.new_id();
                        api.collections.push(Collection {
                            id,
                            name,
                            items: Vec::new(),
                        });
                        api.open.insert(id);
                        api.save_collection(id);
                    }
                    NameFor::NewFolder(c, parent) => {
                        let id = api.new_id();
                        if let Some(list) =
                            api.collection_mut(c).and_then(|col| col.items_of(parent))
                        {
                            list.push(Item::Folder(Folder {
                                id,
                                name,
                                items: Vec::new(),
                            }));
                            api.open.insert(c);
                            api.open.insert(id);
                            api.save_collection(c);
                        }
                    }
                    NameFor::Rename(c, id) => {
                        if id == 0 {
                            if let Some(col) = api.collection_mut(c) {
                                col.name = name;
                            }
                        } else if let Some(col) = api.collection_mut(c) {
                            col.rename(id, &name);
                        }
                        api.save_collection(c);
                        // A tab of that request takes the new name.
                        let new = api
                            .collection(c)
                            .and_then(|col| col.request(id))
                            .map(|r| r.name.clone());
                        let mut renamed = Vec::new();
                        if let Some(new) = new {
                            for (tab_id, tab) in api.tabs.iter_mut() {
                                if tab.origin == Some((c, id)) {
                                    tab.request.name = new.clone();
                                    tab.kept.name = new.clone();
                                    renamed.push((tab_id.clone(), tab.title()));
                                }
                            }
                        }
                        drop(api);
                        for (tab_id, title) in renamed {
                            let _ = host.open_tab(&tab_id, title);
                            self.draw_tab(host, &tab_id);
                        }
                    }
                    NameFor::NewEnvironment => {
                        let id = api.new_id();
                        api.environments.push(Environment {
                            id,
                            name,
                            vars: Vec::new(),
                        });
                        api.editing_env = Some(id);
                        api.save_environments();
                        api.dialog = Some(Dialog::Environments);
                    }
                }
            }
            Dialog::Delete(what) => {
                api.dialog = None;
                match what {
                    Removal::Item(c, 0) => {
                        api.collections.retain(|col| col.id != c);
                        store::delete_collection(c);
                        api.picked = None;
                    }
                    Removal::Item(c, id) => {
                        if let Some(col) = api.collection_mut(c) {
                            col.remove(id);
                        }
                        api.save_collection(c);
                        api.picked = None;
                    }
                    Removal::Environment(id) => {
                        if let Some(env) = api.environment(id).cloned() {
                            store::forget_secrets(&env);
                        }
                        api.environments.retain(|e| e.id != id);
                        if api.active_env == Some(id) {
                            api.active_env = None;
                        }
                        if api.editing_env == Some(id) {
                            api.editing_env = api.environments.first().map(|e| e.id);
                        }
                        api.save_environments();
                        api.dialog = Some(Dialog::Environments);
                    }
                }
                api.release_orphans();
            }
            Dialog::Curl { .. } => match curl::parse(&api.curl) {
                Ok(request) => {
                    api.dialog = None;
                    drop(api);
                    self.open(host, request, None);
                }
                Err(error) => api.dialog = Some(Dialog::Curl { error: Some(error) }),
            },
            _ => api.dialog = None,
        }
    }

    /// The window is closed, whichever way; the environments are kept.
    fn dialog_close(&self) {
        let mut api = self.api();
        if matches!(api.dialog, Some(Dialog::Environments)) {
            api.save_environments();
        }
        api.dialog = None;
    }

    /// What was done in the panel.
    fn panel_event(&self, host: &Host, event: &UiEvent) {
        let text = || event.text().unwrap_or_default().to_string();
        let action = event.action.as_str();
        let (head, rest) = action.split_once(':').unwrap_or((action, ""));
        // An item of the context menu of a row: it acts on that row.
        if let Some(row) = event.menu_item().and_then(tree_row) {
            self.api().picked = Some(row);
        }
        match (head, rest) {
            ("side", "collections") => self.api().side = Side::Collections,
            ("side", "history") => self.api().side = Side::History,
            ("filter", _) => self.api().filter = text(),
            ("new_request", _) => {
                self.open(host, Request::new(""), None);
                return;
            }
            ("new_collection", _) => {
                self.api().dialog = Some(Dialog::Name {
                    what: NameFor::NewCollection,
                    text: String::new(),
                })
            }
            ("import", _) => self.import(host),
            ("import_curl", _) => {
                let mut api = self.api();
                api.curl.clear();
                api.dialog = Some(Dialog::Curl { error: None });
            }
            ("envs", _) => {
                let mut api = self.api();
                api.editing_env = api
                    .active_env
                    .or_else(|| api.environments.first().map(|e| e.id));
                api.dialog = Some(Dialog::Environments);
            }
            ("env", _) => {
                let mut api = self.api();
                let name = text();
                api.active_env = api
                    .environments
                    .iter()
                    .find(|e| e.name == name)
                    .map(|e| e.id);
                api.save_environments();
            }
            ("tree", _) => {
                let Some((item, kind)) = event.tree_item() else {
                    return;
                };
                let Some((c, id)) = tree_row(item) else {
                    return;
                };
                let folder_id = if id == 0 { c } else { id };
                if item.starts_with("r:") {
                    let request = self
                        .api()
                        .collection(c)
                        .and_then(|col| col.request(id))
                        .cloned();
                    self.api().picked = Some((c, id));
                    if let Some(request) = request {
                        if kind == ardoise_plugin::protocol::TreeAction::Click {
                            self.open(host, request, Some((c, id)));
                        }
                    }
                } else {
                    let mut api = self.api();
                    let toggles = kind == ardoise_plugin::protocol::TreeAction::Toggle
                        || api.picked == Some((c, id));
                    if toggles && !api.open.remove(&folder_id) {
                        api.open.insert(folder_id);
                    }
                    api.picked = Some((c, id));
                }
            }
            ("new_in", _) => {
                let Some((c, folder)) = self.api().picked else {
                    return;
                };
                let request = {
                    let mut api = self.api();
                    let id = api.new_id();
                    let mut request = Request::new(&t!("api.untitled"));
                    request.id = id;
                    let Some(list) = api.collection_mut(c).and_then(|col| col.items_of(folder))
                    else {
                        return;
                    };
                    list.push(Item::Request(request.clone()));
                    api.open.insert(c);
                    if folder != 0 {
                        api.open.insert(folder);
                    }
                    api.save_collection(c);
                    (request, id)
                };
                self.open(host, request.0, Some((c, request.1)));
            }
            ("new_folder", _) => {
                let mut api = self.api();
                if let Some((c, id)) = api.picked {
                    api.dialog = Some(Dialog::Name {
                        what: NameFor::NewFolder(c, id),
                        text: String::new(),
                    });
                }
            }
            ("rename", _) => {
                let mut api = self.api();
                if let Some((c, id)) = api.picked {
                    let name = api.collection(c).and_then(|col| {
                        if id == 0 {
                            Some(col.name.clone())
                        } else {
                            state::folder_of(col, id)
                                .map(|f| f.name.clone())
                                .or_else(|| col.request(id).map(|r| r.title()))
                        }
                    });
                    if let Some(text) = name {
                        api.dialog = Some(Dialog::Name {
                            what: NameFor::Rename(c, id),
                            text,
                        });
                    }
                }
            }
            ("delete", _) => {
                let mut api = self.api();
                if let Some((c, id)) = api.picked {
                    api.dialog = Some(Dialog::Delete(Removal::Item(c, id)));
                }
            }
            ("open_picked", _) => {
                let picked = self.api().picked;
                if let Some((c, id)) = picked {
                    let request = self
                        .api()
                        .collection(c)
                        .and_then(|col| col.request(id))
                        .cloned();
                    if let Some(request) = request {
                        self.open(host, request, Some((c, id)));
                    }
                }
                return;
            }
            ("duplicate", _) => {
                let mut api = self.api();
                if let Some((c, id)) = api.picked {
                    let new_id = api.new_id();
                    let suffix = t!("api.copy_suffix").to_string();
                    if api
                        .collection_mut(c)
                        .and_then(|col| col.duplicate(id, new_id, &suffix))
                        .is_some()
                    {
                        api.save_collection(c);
                    }
                }
            }
            ("copy_curl_of", _) => {
                let api = self.api();
                if let Some((c, id)) = api.picked {
                    if let Some(request) = api.collection(c).and_then(|col| col.request(id)) {
                        host.copy(curl::export(&request.resolve(&api.variables())));
                        host.notify(t!("api.curl_copied").to_string());
                    }
                }
                return;
            }
            ("run", _) => {
                let picked = self.api().picked;
                if let Some((c, _)) = picked {
                    self.run_collection(host, c);
                }
            }
            ("cancel_run", _) => {
                self.api().running = None;
                host.notify(t!("api.run_stopped").to_string());
            }
            ("export", _) => {
                let picked = self.api().picked;
                let exported = picked.and_then(|(c, _)| {
                    self.api().collection(c).map(|col| {
                        (
                            format!("{}.postman_collection.json", col.name),
                            postman::export_collection(col),
                        )
                    })
                });
                if let Some((name, text)) = exported {
                    self.export(host, name, text);
                }
            }
            ("history", _) => {
                let at = text()
                    .strip_prefix("h:")
                    .and_then(|n| n.parse::<usize>().ok());
                let request = at.and_then(|i| self.api().history.get(i).map(|e| e.request.clone()));
                if let Some(mut request) = request {
                    request.id = 0;
                    self.open(host, request, None);
                }
                return;
            }
            ("clear_history", _) => {
                let mut api = self.api();
                api.history.clear();
                store::save_history(&api.history);
            }
            ("dialog_text", _) => {
                if let Some(Dialog::Name { text: value, .. }) = self.api().dialog.as_mut() {
                    *value = text();
                }
                return;
            }
            ("curl", _) => {
                let mut api = self.api();
                api.curl = text();
                if let Some(Dialog::Curl { error }) = api.dialog.as_mut() {
                    *error = None;
                }
                return;
            }
            ("dialog_ok", _) => self.dialog_ok(host),
            ("dialog_cancel", _) => self.dialog_close(),
            // The environments.
            ("edit_env", _) => {
                let mut api = self.api();
                let name = text();
                api.editing_env = api
                    .environments
                    .iter()
                    .find(|e| e.name == name)
                    .map(|e| e.id);
            }
            ("env_new", _) => {
                self.api().dialog = Some(Dialog::Name {
                    what: NameFor::NewEnvironment,
                    text: String::new(),
                })
            }
            ("env_name", _) => {
                let mut api = self.api();
                if let Some(env) = api.editing_env.and_then(|id| api.environment_mut(id)) {
                    env.name = text();
                }
                return;
            }
            ("env_dup", _) => {
                let mut api = self.api();
                if let Some(mut copy) = api.editing_env.and_then(|id| api.environment(id)).cloned()
                {
                    copy.id = api.new_id();
                    copy.name = format!("{} {}", copy.name, t!("api.copy_suffix"));
                    api.editing_env = Some(copy.id);
                    api.environments.push(copy);
                    api.save_environments();
                }
            }
            ("env_del", _) => {
                let mut api = self.api();
                if let Some(id) = api.editing_env {
                    api.dialog = Some(Dialog::Delete(Removal::Environment(id)));
                }
            }
            ("env_export", _) => {
                let exported = {
                    let api = self.api();
                    api.editing_env
                        .and_then(|id| api.environment(id))
                        .map(|env| {
                            (
                                format!("{}.postman_environment.json", env.name),
                                postman::export_environment(env, false),
                            )
                        })
                };
                if let Some((name, text)) = exported {
                    self.export(host, name, text);
                }
            }
            ("var_add", _) => {
                let mut api = self.api();
                if let Some(env) = api.editing_env.and_then(|id| api.environment_mut(id)) {
                    env.vars.push(Variable::default());
                }
            }
            (field @ ("var_key" | "var_value" | "var_on" | "var_secret" | "var_del"), i) => {
                let Ok(i) = i.parse::<usize>() else {
                    return;
                };
                let mut api = self.api();
                let Some(env_id) = api.editing_env else {
                    return;
                };
                let Some(env) = api.environment_mut(env_id) else {
                    return;
                };
                match field {
                    "var_del" => {
                        if i < env.vars.len() {
                            let v = env.vars.remove(i);
                            if v.secret {
                                store::forget_secret(env_id, &v.key);
                            }
                        }
                    }
                    _ => {
                        let Some(v) = env.vars.get_mut(i) else {
                            return;
                        };
                        match field {
                            "var_key" => {
                                if v.secret {
                                    store::forget_secret(env_id, &v.key);
                                }
                                v.key = text();
                                return;
                            }
                            "var_value" => {
                                v.value = text();
                                return;
                            }
                            "var_on" => v.enabled = event.checked().unwrap_or(false),
                            _ => {
                                let on = event.checked().unwrap_or(false);
                                if v.secret && !on {
                                    store::forget_secret(env_id, &v.key);
                                }
                                v.secret = on;
                            }
                        }
                    }
                }
            }
            _ => return,
        }
        self.draw_panel(host);
    }

    /// What was done in a tab of a request.
    fn tab_event(&self, host: &Host, id: &str, event: &UiEvent) {
        let text = event.text().unwrap_or_default().to_string();
        let checked = event.checked().unwrap_or(false);
        let action = event.action.as_str();
        let mut parts = action.splitn(3, ':');
        let (head, a, b) = (
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
        );
        let index = |s: &str| s.parse::<usize>().ok();
        // Typing in a field: the tab is not drawn again at each key.
        let mut redraw = true;
        match head {
            "send" => return self.send(host, id),
            "save" => return self.save(host, id),
            "resp_search_next" => {
                // From the cursor of the body (after the match selected).
                let at = host.editor_state(id, "resp").ok();
                let mut api = self.api();
                if let Some(view) = api.tabs.get_mut(id).and_then(|t| t.response.as_mut()) {
                    view.search = text;
                    let from = match at {
                        Some(s) => {
                            crate::state::offset_of(&view.shown, s.cursor.line, s.cursor.column)
                        }
                        None => view.found.map_or(0, |(_, end)| end),
                    };
                    view.found = crate::state::find_next(&view.shown, &view.search, from);
                }
            }
            "copy_curl" => {
                let api = self.api();
                if let Some(tab) = api.tabs.get(id) {
                    host.copy(curl::export(&tab.request.resolve(&api.variables())));
                    host.notify(t!("api.curl_copied").to_string());
                }
                return;
            }
            "copy_body" => {
                let api = self.api();
                if let Some(view) = api.tabs.get(id).and_then(|t| t.response.as_ref()) {
                    let text = match (&view.pretty, view.show_pretty) {
                        (Some(pretty), true) => pretty.clone(),
                        _ => view.response.text(),
                    };
                    host.copy(text);
                }
                return;
            }
            "save_body" => {
                let saved = {
                    let api = self.api();
                    api.tabs
                        .get(id)
                        .and_then(|t| t.response.as_ref())
                        .map(|view| {
                            let ext = match view.kind {
                                http::Kind::Json => "json",
                                http::Kind::Xml => "xml",
                                http::Kind::Html => "html",
                                http::Kind::Text => "txt",
                                http::Kind::Binary => "bin",
                            };
                            (
                                format!("response.{ext}"),
                                view.response.body.clone(),
                                view.response.truncated,
                            )
                        })
                };
                if let Some((name, bytes, cut)) = saved {
                    match host.save_bytes(name, bytes) {
                        Ok(true) if cut => host.notify(
                            t!("api.truncated", size = ui::size(http::MAX_BODY)).to_string(),
                        ),
                        Ok(_) => {}
                        Err(e) => host.error(e.message),
                    }
                }
                return;
            }
            "body_file" => {
                if let Ok(Some(path)) = host.pick_file(t!("api.choose_file"), &[]) {
                    if let Some(tab) = self.api().tabs.get_mut(id) {
                        tab.request.body.file = path.to_string_lossy().into_owned();
                    }
                }
            }
            _ => {
                let mut api = self.api();
                let Some(tab) = api.tabs.get_mut(id) else {
                    return;
                };
                let r = &mut tab.request;
                match head {
                    "name" => {
                        r.name = text;
                        redraw = false;
                    }
                    "url" => {
                        r.url = text;
                        redraw = false;
                    }
                    "method" => {
                        if let Some(m) = Method::ALL.iter().find(|m| m.as_str() == text) {
                            r.method = *m;
                        }
                    }
                    "cancel" => {
                        tab.sending = false;
                        tab.token += 1;
                        tab.notice = Some(t!("api.cancelled").to_string());
                    }
                    "save_target" => {
                        let target = api
                            .collections
                            .iter()
                            .find(|c| c.name == text)
                            .map(|c| c.id);
                        if let Some(tab) = api.tabs.get_mut(id) {
                            tab.save_target = target;
                        }
                    }
                    "sec" => {
                        tab.section = match a {
                            "headers" => ReqSection::Headers,
                            "body" => ReqSection::Body,
                            "auth" => ReqSection::Auth,
                            "settings" => ReqSection::Settings,
                            "tests" => ReqSection::Tests,
                            _ => ReqSection::Params,
                        }
                    }
                    "padd" => {
                        if let Some(list) = pairs_mut(r, a) {
                            list.push(Pair::default());
                        }
                    }
                    "pdel" => {
                        if let (Some(list), Some(i)) = (pairs_mut(r, a), index(b)) {
                            if i < list.len() {
                                list.remove(i);
                            }
                        }
                    }
                    "pk" | "pv" | "pon" => {
                        if let Some(p) =
                            pairs_mut(r, a).and_then(|l| index(b).and_then(|i| l.get_mut(i)))
                        {
                            match head {
                                "pk" => {
                                    p.key = text;
                                    redraw = false;
                                }
                                "pv" => {
                                    p.value = text;
                                    redraw = false;
                                }
                                _ => p.enabled = checked,
                            }
                        }
                    }
                    "preset" => r.headers.push(Pair::new(&text, "")),
                    "body_kind" => {
                        if let Some((k, _)) = ui::body_kinds().into_iter().find(|(_, l)| *l == text)
                        {
                            r.body.kind = k;
                        }
                    }
                    "body" => {
                        r.body.text = text.strip_suffix('\n').unwrap_or(&text).to_string();
                        redraw = false;
                    }
                    "body_format" => {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&r.body.text) {
                            if let Ok(laid) = serde_json::to_string_pretty(&value) {
                                r.body.text = laid;
                            }
                        }
                    }
                    "auth_kind" => {
                        if let Some((k, _)) = ui::auth_kinds().into_iter().find(|(_, l)| *l == text)
                        {
                            r.auth.kind = k;
                        }
                    }
                    "auth_token" | "auth_user" | "auth_pass" | "auth_key_name"
                    | "auth_key_value" => {
                        let a = &mut r.auth;
                        match head {
                            "auth_token" => a.token = text,
                            "auth_user" => a.username = text,
                            "auth_pass" => a.password = text,
                            "auth_key_name" => a.key_name = text,
                            _ => a.key_value = text,
                        }
                        redraw = false;
                    }
                    "key_in_header" => r.auth.key_in_header = checked,
                    "timeout" => {
                        let digits: String =
                            text.chars().filter(char::is_ascii_digit).take(5).collect();
                        r.settings.timeout_secs = digits.parse().unwrap_or(0);
                        redraw = false;
                    }
                    "redirects" => r.settings.follow_redirects = checked,
                    "verify_tls" => r.settings.verify_tls = checked,
                    "ck_add" => r.checks.push(model::Check::default()),
                    "ck_del" => {
                        if let Some(i) = index(a).filter(|i| *i < r.checks.len()) {
                            r.checks.remove(i);
                        }
                    }
                    "ck_on" | "ck_src" | "ck_name" | "ck_op" | "ck_exp" => {
                        if let Some(c) = index(a).and_then(|i| r.checks.get_mut(i)) {
                            match head {
                                "ck_on" => c.enabled = checked,
                                "ck_src" => {
                                    if let Some(s) =
                                        CheckSource::ALL.iter().find(|s| s.to_string() == text)
                                    {
                                        c.source = *s;
                                    }
                                }
                                "ck_op" => {
                                    if let Some(o) =
                                        CheckOp::ALL.iter().find(|o| o.to_string() == text)
                                    {
                                        c.op = *o;
                                    }
                                }
                                "ck_name" => {
                                    c.name = text;
                                    redraw = false;
                                }
                                _ => {
                                    c.expected = text;
                                    redraw = false;
                                }
                            }
                        }
                    }
                    "ex_add" => r.extracts.push(model::Extract::default()),
                    "ex_del" => {
                        if let Some(i) = index(a).filter(|i| *i < r.extracts.len()) {
                            r.extracts.remove(i);
                        }
                    }
                    "ex_on" | "ex_var" | "ex_from" | "ex_path" => {
                        if let Some(e) = index(a).and_then(|i| r.extracts.get_mut(i)) {
                            match head {
                                "ex_on" => e.enabled = checked,
                                "ex_from" => {
                                    if let Some(s) =
                                        ExtractSource::ALL.iter().find(|s| s.to_string() == text)
                                    {
                                        e.source = *s;
                                    }
                                }
                                "ex_var" => {
                                    e.variable = text;
                                    redraw = false;
                                }
                                _ => {
                                    e.path = text;
                                    redraw = false;
                                }
                            }
                        }
                    }
                    "rsec" => {
                        tab.resp_section = match a {
                            "headers" => RespSection::Headers,
                            "cookies" => RespSection::Cookies,
                            "tests" => RespSection::Tests,
                            _ => RespSection::Body,
                        }
                    }
                    "pretty" => {
                        if let Some(view) = tab.response.as_mut() {
                            if view.pretty.is_some() {
                                view.show_pretty = checked;
                                view.show();
                            }
                        }
                    }
                    "resp_search" => {
                        if let Some(view) = tab.response.as_mut() {
                            view.search = text;
                            view.found = None;
                        }
                    }
                    _ => return,
                }
            }
        }
        // The name of the tab follows the request; its mark, the changes.
        let (title, dirty) = {
            let api = self.api();
            match api.tabs.get(id) {
                Some(tab) => (tab.title(), tab.changed()),
                None => return,
            }
        };
        if head == "name" || head == "url" || head == "method" {
            let _ = host.open_tab(id, title);
        }
        host.tab_dirty(id, dirty);
        if redraw {
            self.draw_tab(host, id);
        }
        self.keep_tabs();
    }

    fn settings_view(&self) -> Node {
        column([
            text(t!("api.network")).style(TextStyle::Bold),
            input("ca_file", &self.network.ca_file, "/path/ca.pem"),
            text(t!("api.network_ca")).style(TextStyle::Muted),
            input("client_cert", &self.network.client_cert, "/path/client.pem"),
            text(t!("api.network_cert")).style(TextStyle::Muted),
            input("proxy", &self.network.proxy, "http://proxy:3128"),
            text(t!("api.network_proxy")).style(TextStyle::Muted),
        ])
        .spacing(6.0)
    }
}

impl Plugin for ApiPlugin {
    fn initialize(&mut self, _: &Host, params: &InitializeParams) {
        rust_i18n::set_locale(&params.settings.locale);
        self.data_dir = params.data_dir.clone();
        fsutil::set_data_dir(self.data_dir.clone());
        self.network = std::fs::read_to_string(self.data_dir.join("settings.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        self.network.apply();
        let mut api = Api::load();
        // The requests open in tabs the last time.
        let kept: KeptTabs = std::fs::read_to_string(self.tabs_file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        api.counter = kept.counter;
        api.tabs = kept.tabs;
        api.release_orphans();
        *self.api() = api;
    }

    fn command(&mut self, host: &Host, command: &str, _: &Context) {
        match command {
            "api.new_request" => self.open(host, Request::new(""), None),
            "api.show" => {
                host.show_panel(PANEL);
                self.draw_panel(host);
            }
            "api.import_curl" => {
                {
                    let mut api = self.api();
                    api.curl.clear();
                    api.dialog = Some(Dialog::Curl { error: None });
                }
                host.show_panel(PANEL);
                self.draw_panel(host);
            }
            _ => {}
        }
    }

    fn panel_opened(&mut self, host: &Host, panel: &str) {
        match panel {
            PANEL => self.draw_panel(host),
            SETTINGS => host.render(SETTINGS, self.settings_view()),
            // A tab that came back with the session.
            tab => {
                if self.api().tabs.contains_key(tab) {
                    self.draw_tab(host, tab);
                }
            }
        }
    }

    fn ui_event(&mut self, host: &Host, event: &UiEvent) {
        match event.panel.as_str() {
            PANEL => self.panel_event(host, event),
            SETTINGS => {
                let value = event.text().unwrap_or_default().to_string();
                match event.action.as_str() {
                    "ca_file" => self.network.ca_file = value,
                    "client_cert" => self.network.client_cert = value,
                    "proxy" => self.network.proxy = value,
                    _ => return,
                }
                self.network.apply();
                if let Ok(json) = serde_json::to_string_pretty(&self.network) {
                    let _ = fsutil::write_atomic(
                        &self.data_dir.join("settings.json"),
                        json.as_bytes(),
                        false,
                    );
                }
            }
            tab => self.tab_event(host, tab, event),
        }
    }

    fn tab_save(&mut self, host: &Host, tab: &str) {
        self.save(host, tab);
    }

    fn tab_closed(&mut self, _: &Host, tab: &str) {
        self.api().tabs.remove(tab);
        self.keep_tabs();
    }

    fn settings_changed(&mut self, host: &Host, settings: &EditorSettings) {
        rust_i18n::set_locale(&settings.locale);
        self.draw_panel(host);
        let ids: Vec<String> = self.api().tabs.keys().cloned().collect();
        for id in ids {
            self.draw_tab(host, &id);
        }
    }

    fn forget_data(&mut self, _: &Host) -> Result<(), String> {
        self.forgotten = true;
        let api = self.api();
        api::store::forget_every_secret(&api.collections, &api.environments);
        Ok(())
    }

    fn shutdown(&mut self, _: &Host) {
        if self.forgotten {
            return;
        }
        self.api().save_environments_now();
        self.keep_tabs();
    }
}

fn main() -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    run(ApiPlugin {
        shared: Arc::new(Mutex::new(Api::empty())),
        network: NetworkSettings::default(),
        data_dir: std::path::PathBuf::new(),
        runtime,
        forgotten: false,
    })
}
