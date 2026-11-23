//! What the panel and the tabs of requests show, as nodes for the editor.

use ardoise_plugin::ui::*;

use crate::api::http::{self, Class};
use crate::api::model::{
    AuthKind, BodyKind, CheckOp, CheckSource, Collection, ExtractSource, Item, Method, Pair,
};
use crate::state::{Api, Dialog, NameFor, ReqSection, ReqTab, RespSection, Side};

/// The labels of the kinds of body, in order.
pub fn body_kinds() -> Vec<(BodyKind, String)> {
    BodyKind::ALL
        .iter()
        .map(|k| {
            let label = match k {
                BodyKind::None => t!("api.body_none"),
                BodyKind::Json => t!("api.body_json"),
                BodyKind::Text => t!("api.body_text"),
                BodyKind::Form => t!("api.body_form"),
                BodyKind::Multipart => t!("api.body_multipart"),
                BodyKind::File => t!("api.body_file"),
            };
            (*k, label.to_string())
        })
        .collect()
}

pub fn auth_kinds() -> Vec<(AuthKind, String)> {
    AuthKind::ALL
        .iter()
        .map(|k| {
            let label = match k {
                AuthKind::None => t!("api.auth_none"),
                AuthKind::Bearer => t!("api.auth_bearer"),
                AuthKind::Basic => t!("api.auth_basic"),
                AuthKind::ApiKey => t!("api.auth_key"),
            };
            (*k, label.to_string())
        })
        .collect()
}

/// The usual headers, offered to be added.
pub const PRESETS: [&str; 7] = [
    "Accept",
    "Authorization",
    "Content-Type",
    "Cache-Control",
    "Cookie",
    "User-Agent",
    "X-Requested-With",
];

/// A size in words: `1.2 MB`.
pub fn size(bytes: usize) -> String {
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{:.1} KB", b / 1024.0)
    } else {
        format!("{:.1} MB", b / (1024.0 * 1024.0))
    }
}

fn ghost(label: impl Into<String>, action: impl Into<String>) -> Node {
    button(label, action).button_style(ButtonStyle::Ghost)
}

fn muted(s: impl Into<String>) -> Node {
    text(s).style(TextStyle::Muted)
}

// ─── The panel ───────────────────────────────────────────────────────────

fn matches(item: &Item, needle: &str) -> bool {
    match item {
        Item::Request(r) => {
            r.title().to_lowercase().contains(needle) || r.url.to_lowercase().contains(needle)
        }
        Item::Folder(f) => {
            f.name.to_lowercase().contains(needle) || f.items.iter().any(|i| matches(i, needle))
        }
    }
}

fn tree_items(api: &Api, collection: &Collection, items: &[Item], needle: &str) -> Vec<TreeItem> {
    items
        .iter()
        .filter(|i| needle.is_empty() || matches(i, needle))
        .map(|item| match item {
            Item::Request(r) => tree_item(format!("r:{}:{}", collection.id, r.id), r.title())
                .icon("\u{f0ec}")
                .detail(r.method.as_str())
                .menu(item_menu(api, collection, r.id)),
            Item::Folder(f) => tree_item(format!("f:{}:{}", collection.id, f.id), f.name.clone())
                .icon("\u{f07b}")
                .expandable()
                .expanded(api.is_open(f.id))
                .children(tree_items(api, collection, &f.items, needle))
                .menu(item_menu(api, collection, f.id)),
        })
        .collect()
}

/// The context menu of a row of the tree: a collection (`id` 0), a folder
/// or a request of `collection`.
fn item_menu(api: &Api, collection: &Collection, id: u64) -> Vec<MenuItem> {
    let request = id != 0 && collection.request(id).is_some();
    let mut items = Vec::new();
    if !request {
        items.push(MenuItem::new(t!("api.new_request"), "new_in"));
        items.push(MenuItem::new(t!("api.new_folder"), "new_folder"));
    }
    items.push(MenuItem::new(t!("api.rename"), "rename"));
    if request {
        items.push(MenuItem::new(t!("api.open"), "open_picked"));
        items.push(MenuItem::new(t!("api.duplicate"), "duplicate"));
        items.push(MenuItem::new(t!("api.copy_curl"), "copy_curl_of"));
    }
    if id == 0 {
        if api.running.is_some() {
            items.push(MenuItem::new(t!("api.run_stop"), "cancel_run"));
        } else {
            items.push(MenuItem::new(t!("api.run_collection"), "run"));
        }
        items.push(MenuItem::new(t!("api.export"), "export"));
    }
    items.push(MenuItem::new(t!("api.delete"), "delete"));
    items
}

pub fn panel(api: &Api) -> Node {
    if let Some(dialog) = &api.dialog {
        return dialog_view(api, dialog);
    }
    let mut env_names = vec![t!("api.no_env").to_string()];
    env_names.extend(api.environments.iter().map(|e| e.name.clone()));
    let active = api
        .active_env
        .and_then(|id| api.environment(id))
        .map_or_else(|| t!("api.no_env").to_string(), |e| e.name.clone());
    let mut body = vec![
        // The tools are pictos with their tooltip (the words did not fit in a
        // narrow sidebar); the new request keeps its words.
        row([
            ghost(t!("api.new_request"), "new_request")
                .icon("plus")
                .with_label(),
            fill(),
            ghost(t!("api.new_collection"), "new_collection").icon("folder-plus"),
            ghost(t!("api.import"), "import").icon("download"),
            ghost(t!("api.import_curl"), "import_curl").icon("terminal"),
        ])
        .spacing(4.0),
        row([
            select("env", env_names, Some(active)),
            ghost(t!("api.environments"), "envs").icon("sliders-horizontal"),
        ])
        .spacing(4.0),
        row([
            button(t!("api.collections"), "side:collections").button_style(
                if api.side == Side::Collections {
                    ButtonStyle::Outline
                } else {
                    ButtonStyle::Ghost
                },
            ),
            button(t!("api.history"), "side:history").button_style(if api.side == Side::History {
                ButtonStyle::Outline
            } else {
                ButtonStyle::Ghost
            }),
        ])
        .spacing(4.0),
    ];
    match api.side {
        Side::Collections => {
            body.push(input("filter", &api.filter, t!("api.filter")));
            if api.collections.is_empty() {
                body.push(muted(t!("api.empty")));
            }
            let needle = api.filter.trim().to_lowercase();
            let items: Vec<TreeItem> = api
                .collections
                .iter()
                .map(|c| {
                    tree_item(format!("c:{}:0", c.id), c.name.clone())
                        .icon("\u{f1b3}")
                        .expandable()
                        .expanded(api.is_open(c.id))
                        .children(tree_items(api, c, &c.items, &needle))
                        .menu(item_menu(api, c, 0))
                })
                .collect();
            body.push(scroll(tree("tree", items)));
        }
        Side::History => {
            if api.history.is_empty() {
                body.push(muted(t!("api.history_empty")));
            } else {
                body.push(ghost(t!("api.history_clear"), "clear_history"));
            }
            let items = api.history.iter().enumerate().map(|(i, h)| {
                let how = match (&h.error, h.status) {
                    (Some(_), _) => "✗".to_string(),
                    (None, Some(s)) => format!("{s} · {} ms", h.ms),
                    _ => String::new(),
                };
                item(
                    format!("h:{i}"),
                    format!("{} {}", h.request.method.as_str(), h.request.title()),
                )
                .detail(format!("{} · {how}", h.time))
            });
            body.push(scroll(list("history", items)));
        }
    }
    if !api.set_aside.is_empty() {
        body.push(text(api.set_aside.join(", ")).style(TextStyle::Error));
    }
    column(body).spacing(8.0)
}

fn dialog_view(api: &Api, dialog: &Dialog) -> Node {
    let ok_cancel = |ok: &str| {
        row([
            ghost(t!("api.cancel"), "dialog_cancel"),
            button(ok.to_string(), "dialog_ok").button_style(ButtonStyle::Primary),
        ])
        .spacing(8.0)
    };
    match dialog {
        Dialog::Name { what, text: value } => {
            let title = match what {
                NameFor::NewCollection => t!("api.new_collection"),
                NameFor::NewFolder(..) => t!("api.new_folder"),
                NameFor::Rename(..) => t!("api.rename"),
                NameFor::NewEnvironment => t!("api.new_environment"),
            };
            column([
                text(title.to_string()).style(TextStyle::Heading),
                input("dialog_text", value, t!("api.name_hint")).on_submit("dialog_ok"),
                ok_cancel(&t!("api.ok")),
            ])
            .spacing(8.0)
        }
        Dialog::Delete(what) => {
            let question = match what {
                crate::state::Removal::Item(c, 0) => {
                    let name = api
                        .collection(*c)
                        .map(|c| c.name.clone())
                        .unwrap_or_default();
                    t!("api.delete_collection", name = name.as_str()).to_string()
                }
                crate::state::Removal::Item(..) => t!("api.delete_item").to_string(),
                crate::state::Removal::Environment(id) => {
                    let name = api
                        .environment(*id)
                        .map(|e| e.name.clone())
                        .unwrap_or_default();
                    t!("api.delete_environment", name = name.as_str()).to_string()
                }
            };
            column([
                text(question),
                row([
                    ghost(t!("api.cancel"), "dialog_cancel"),
                    button(t!("api.delete"), "dialog_ok").button_style(ButtonStyle::Danger),
                ])
                .spacing(8.0),
            ])
            .spacing(8.0)
        }
        Dialog::Curl { error } => {
            let mut rows = vec![
                text(t!("api.import_curl")).style(TextStyle::Heading),
                muted(t!("api.curl_hint")),
                editor("curl", &api.curl, "sh").height(180.0),
            ];
            if let Some(e) = error {
                rows.push(text(e.clone()).style(TextStyle::Error));
            }
            rows.push(ok_cancel(&t!("api.ok")));
            column(rows).spacing(8.0)
        }
        Dialog::Report(lines) => column([
            text(t!("api.import_report")).style(TextStyle::Heading),
            scroll(column(
                lines.iter().map(|l| text(l.clone()).style(TextStyle::Mono)),
            )),
            button(t!("api.ok"), "dialog_cancel").button_style(ButtonStyle::Primary),
        ])
        .spacing(8.0),
        Dialog::Environments => environments(api),
    }
}

fn environments(api: &Api) -> Node {
    let mut rows = vec![text(t!("api.environments")).style(TextStyle::Heading)];
    let names: Vec<String> = api.environments.iter().map(|e| e.name.clone()).collect();
    let editing = api.editing_env.and_then(|id| api.environment(id));
    rows.push(
        row([
            select("edit_env", names, editing.map(|e| e.name.clone())),
            ghost(format!("+ {}", t!("api.new_environment")), "env_new"),
        ])
        .spacing(6.0),
    );
    match editing {
        None => rows.push(muted(t!("api.env_none"))),
        Some(env) => {
            rows.push(input("env_name", &env.name, t!("api.name_hint")));
            for (i, v) in env.vars.iter().enumerate() {
                let mut value = input(format!("var_value:{i}"), &v.value, t!("api.value"));
                if v.secret {
                    value = value.secret();
                }
                rows.push(
                    row([
                        checkbox(format!("var_on:{i}"), "", v.enabled),
                        input(format!("var_key:{i}"), &v.key, t!("api.key")),
                        value,
                        checkbox(format!("var_secret:{i}"), t!("api.secret"), v.secret),
                        ghost("×", format!("var_del:{i}")),
                    ])
                    .spacing(6.0),
                );
            }
            rows.push(ghost(format!("+ {}", t!("api.add_variable")), "var_add"));
            rows.push(muted(t!("api.env_hint")));
            rows.push(
                row([
                    ghost(t!("api.duplicate"), "env_dup"),
                    ghost(t!("api.export"), "env_export"),
                    button(t!("api.delete"), "env_del").button_style(ButtonStyle::Danger),
                ])
                .spacing(6.0),
            );
        }
    }
    rows.push(button(t!("api.ok"), "dialog_cancel").button_style(ButtonStyle::Primary));
    scroll(column(rows).spacing(8.0))
}

// ─── A tab of a request ──────────────────────────────────────────────────

fn table(name: &str, pairs: &[Pair]) -> Node {
    let mut rows: Vec<Node> = pairs
        .iter()
        .enumerate()
        .map(|(i, p)| {
            row([
                checkbox(format!("pon:{name}:{i}"), "", p.enabled),
                input(format!("pk:{name}:{i}"), &p.key, t!("api.key")),
                input(format!("pv:{name}:{i}"), &p.value, t!("api.value")),
                ghost("×", format!("pdel:{name}:{i}")),
            ])
            .spacing(6.0)
        })
        .collect();
    rows.push(ghost(
        format!("+ {}", t!("api.add")),
        format!("padd:{name}"),
    ));
    column(rows).spacing(4.0)
}

fn section_button(label: String, section: ReqSection, current: ReqSection, id: &str) -> Node {
    button(label, format!("sec:{id}")).button_style(if section == current {
        ButtonStyle::Outline
    } else {
        ButtonStyle::Ghost
    })
}

fn request_part(api: &Api, tab: &ReqTab) -> Node {
    let r = &tab.request;
    match tab.section {
        ReqSection::Params => table("params", &r.params),
        ReqSection::Headers => column([
            table("headers", &r.headers),
            select("preset", PRESETS.iter().map(|s| s.to_string()), None)
                .placeholder(t!("api.common_header")),
        ])
        .spacing(6.0),
        ReqSection::Body => {
            let kinds = body_kinds();
            let current = kinds
                .iter()
                .find(|(k, _)| *k == r.body.kind)
                .map(|(_, l)| l.clone());
            let mut rows = vec![select(
                "body_kind",
                kinds.into_iter().map(|(_, l)| l),
                current,
            )];
            if r.body.kind != BodyKind::None && !r.method.has_body() {
                rows.push(muted(t!("api.body_ignored", method = r.method.as_str())));
            }
            match r.body.kind {
                BodyKind::None => rows.push(muted(t!("api.body_none_hint"))),
                BodyKind::Json | BodyKind::Text => {
                    let json = r.body.kind == BodyKind::Json;
                    rows.push(
                        editor("body", &r.body.text, if json { "json" } else { "" }).height(260.0),
                    );
                    if json {
                        let valid = serde_json::from_str::<serde_json::Value>(&r.body.text).is_ok();
                        rows.push(button(t!("api.format"), "body_format").enabled(valid));
                    }
                }
                BodyKind::Form => rows.push(table("form", &r.body.form)),
                BodyKind::Multipart => {
                    rows.push(muted(t!("api.body_multipart_hint")));
                    rows.push(table("form", &r.body.form));
                }
                BodyKind::File => rows.push(
                    row([
                        button(t!("api.choose_file"), "body_file"),
                        text(r.body.file.clone()).style(TextStyle::Mono),
                    ])
                    .spacing(8.0),
                ),
            }
            column(rows).spacing(6.0)
        }
        ReqSection::Auth => {
            let kinds = auth_kinds();
            let current = kinds
                .iter()
                .find(|(k, _)| *k == r.auth.kind)
                .map(|(_, l)| l.clone());
            let mut rows = vec![select(
                "auth_kind",
                kinds.into_iter().map(|(_, l)| l),
                current,
            )];
            let a = &r.auth;
            match a.kind {
                AuthKind::None => rows.push(muted(t!("api.auth_none_hint"))),
                AuthKind::Bearer => {
                    rows.push(input("auth_token", &a.token, t!("api.token")).secret())
                }
                AuthKind::Basic => {
                    rows.push(input("auth_user", &a.username, t!("api.username")));
                    rows.push(input("auth_pass", &a.password, t!("api.password")).secret());
                }
                AuthKind::ApiKey => {
                    rows.push(input("auth_key_name", &a.key_name, t!("api.key_name")));
                    rows.push(input("auth_key_value", &a.key_value, t!("api.key_value")).secret());
                    rows.push(checkbox(
                        "key_in_header",
                        t!("api.key_in_header"),
                        a.key_in_header,
                    ));
                }
            }
            rows.push(muted(t!("api.auth_variables")));
            column(rows).spacing(6.0)
        }
        ReqSection::Settings => {
            let s = &r.settings;
            column([
                row([
                    text(t!("api.timeout")),
                    input("timeout", s.timeout_secs.to_string(), "30"),
                    muted(t!("api.timeout_hint")),
                ])
                .spacing(8.0),
                checkbox("redirects", t!("api.redirects"), s.follow_redirects),
                checkbox("verify_tls", t!("api.verify_tls"), s.verify_tls),
            ])
            .spacing(6.0)
        }
        ReqSection::Tests => {
            let sources: Vec<String> = CheckSource::ALL.iter().map(|s| s.to_string()).collect();
            let ops: Vec<String> = CheckOp::ALL.iter().map(|o| o.to_string()).collect();
            let froms: Vec<String> = ExtractSource::ALL.iter().map(|s| s.to_string()).collect();
            let mut rows = vec![text(t!("api.checks_title")).style(TextStyle::Bold)];
            for (i, c) in r.checks.iter().enumerate() {
                rows.push(
                    row([
                        checkbox(format!("ck_on:{i}"), "", c.enabled),
                        select(
                            format!("ck_src:{i}"),
                            sources.clone(),
                            Some(c.source.to_string()),
                        ),
                        input(format!("ck_name:{i}"), &c.name, t!("api.check_name")),
                        select(format!("ck_op:{i}"), ops.clone(), Some(c.op.to_string())),
                        input(format!("ck_exp:{i}"), &c.expected, t!("api.expected")),
                        ghost("×", format!("ck_del:{i}")),
                    ])
                    .spacing(6.0),
                );
            }
            rows.push(ghost(format!("+ {}", t!("api.add_check")), "ck_add"));
            rows.push(text(t!("api.extracts_title")).style(TextStyle::Bold));
            for (i, e) in r.extracts.iter().enumerate() {
                rows.push(
                    row([
                        checkbox(format!("ex_on:{i}"), "", e.enabled),
                        input(format!("ex_var:{i}"), &e.variable, t!("api.var_name")),
                        select(
                            format!("ex_from:{i}"),
                            froms.clone(),
                            Some(e.source.to_string()),
                        ),
                        input(format!("ex_path:{i}"), &e.path, t!("api.extract_path")),
                        ghost("×", format!("ex_del:{i}")),
                    ])
                    .spacing(6.0),
                );
            }
            rows.push(ghost(format!("+ {}", t!("api.add_extract")), "ex_add"));
            let _ = api;
            column(rows).spacing(6.0)
        }
    }
}

fn response_part(tab: &ReqTab) -> Node {
    if tab.sending {
        return muted(t!("api.sending"));
    }
    let mut rows = Vec::new();
    if let Some(e) = &tab.error {
        rows.push(text(e.clone()).style(TextStyle::Error));
    }
    if let Some(n) = &tab.notice {
        rows.push(muted(n.clone()));
    }
    let Some(view) = &tab.response else {
        if tab.error.is_none() {
            rows.push(muted(t!("api.no_response")));
        }
        return column(rows).spacing(6.0);
    };
    let r = &view.response;
    let status_style = match http::class_of(r.status) {
        Class::Success => TextStyle::Bold,
        Class::Redirect | Class::Info => TextStyle::Normal,
        _ => TextStyle::Error,
    };
    rows.push(
        row([
            text(format!("{} {}", r.status, r.reason)).style(status_style),
            muted(format!(
                "{} ms · {} · {}",
                r.ms,
                size(r.body.len()),
                r.version
            )),
        ])
        .spacing(10.0),
    );
    let passed = view.outcomes.iter().filter(|o| o.passed).count();
    let tests_label = if view.outcomes.is_empty() {
        t!("api.tests").to_string()
    } else {
        format!("{} {passed}/{}", t!("api.tests"), view.outcomes.len())
    };
    let section = |label: String, s: RespSection, id: &str| {
        button(label, format!("rsec:{id}")).button_style(if tab.resp_section == s {
            ButtonStyle::Outline
        } else {
            ButtonStyle::Ghost
        })
    };
    rows.push(
        row([
            section(t!("api.body").to_string(), RespSection::Body, "body"),
            section(
                format!("{} ({})", t!("api.headers"), r.headers.len()),
                RespSection::Headers,
                "headers",
            ),
            section(
                format!("{} ({})", t!("api.cookies"), r.cookies.len()),
                RespSection::Cookies,
                "cookies",
            ),
            section(tests_label, RespSection::Tests, "tests"),
        ])
        .spacing(4.0),
    );
    match tab.resp_section {
        RespSection::Body => {
            let mut tools = vec![];
            if view.pretty.is_some() {
                tools.push(checkbox("pretty", t!("api.pretty"), view.show_pretty));
            }
            // Enter goes to the next match.
            tools.push(
                input("resp_search", &view.search, t!("api.search")).on_submit("resp_search_next"),
            );
            if !view.search.is_empty() {
                tools.push(muted(t!("api.matches", n = view.matches())));
            }
            tools.push(ghost(t!("api.copy"), "copy_body"));
            tools.push(ghost(t!("api.save_body"), "save_body"));
            rows.push(row(tools).spacing(6.0));
            if r.truncated {
                rows.push(muted(t!("api.truncated", size = size(http::MAX_BODY))));
            }
            if view.kind == http::Kind::Binary {
                rows.push(muted(t!("api.binary", size = size(r.body.len()))));
            } else {
                if view.cut {
                    rows.push(muted(t!("api.cut", size = size(crate::state::MAX_SHOWN))));
                }
                let mut body = editor("resp", &view.shown, view.language()).read_only();
                if let Some((start, end)) = view.found {
                    let at = |offset| {
                        let (line, column) = crate::state::position_of(&view.shown, offset);
                        EditorPos { line, column }
                    };
                    body = body.selection(EditorSelection {
                        anchor: at(start),
                        cursor: at(end),
                    });
                }
                rows.push(body);
            }
        }
        RespSection::Headers => rows.push(scroll(list(
            "resp_headers",
            r.headers
                .iter()
                .enumerate()
                .map(|(i, (k, v))| item(i.to_string(), k.clone()).detail(v.clone())),
        ))),
        RespSection::Cookies => {
            if r.cookies.is_empty() {
                rows.push(muted(t!("api.no_cookies")));
            }
            rows.push(scroll(list(
                "cookies",
                r.cookies
                    .iter()
                    .enumerate()
                    .map(|(i, c)| item(i.to_string(), c.name.clone()).detail(c.value.clone())),
            )));
        }
        RespSection::Tests => {
            if view.outcomes.is_empty() {
                rows.push(muted(t!("api.tests_none")));
            }
            for o in &view.outcomes {
                let mark = if o.passed { "✓" } else { "✗" };
                let line = if o.actual.is_empty() {
                    format!("{mark} {}", o.label)
                } else {
                    format!("{mark} {} — {}", o.label, o.actual)
                };
                rows.push(text(line).style(if o.passed {
                    TextStyle::Normal
                } else {
                    TextStyle::Error
                }));
            }
        }
    }
    column(rows).spacing(6.0)
}

pub fn tab(api: &Api, tab: &ReqTab) -> Node {
    let r = &tab.request;
    let methods: Vec<String> = Method::ALL.iter().map(|m| m.as_str().to_string()).collect();
    let has_url = !r.url.trim().is_empty();
    let mut head = vec![
        input("name", &r.name, t!("api.name_hint")),
        // Nothing to keep or to copy until there is an address.
        ghost(t!("api.save"), "save").enabled(has_url),
        ghost(t!("api.copy_curl"), "copy_curl").enabled(has_url),
    ];
    // A request not kept yet goes to a collection to choose.
    if tab.origin.is_none() && !api.collections.is_empty() {
        let names: Vec<String> = api.collections.iter().map(|c| c.name.clone()).collect();
        let target = tab
            .save_target
            .and_then(|id| api.collection(id))
            .or_else(|| api.collections.first())
            .map(|c| c.name.clone());
        head.push(select("save_target", names, target));
    }
    let send = if tab.sending {
        button(t!("api.cancel"), "cancel").button_style(ButtonStyle::Danger)
    } else {
        button(t!("api.send"), "send")
            .button_style(ButtonStyle::Primary)
            .enabled(has_url)
    };
    let missing = r.missing(&api.variables());
    let mut rows = vec![
        row(head).spacing(6.0),
        row([
            select("method", methods, Some(r.method.as_str().to_string())),
            input("url", &r.url, "https://api.example.com/items").on_submit("send"),
            send,
        ])
        .spacing(6.0),
    ];
    if !missing.is_empty() {
        rows.push(muted(t!(
            "api.missing_vars",
            names = missing.join(", ").as_str()
        )));
    }
    let s = tab.section;
    rows.push(
        row([
            section_button(
                format!("{} ({})", t!("api.params"), r.params.len()),
                ReqSection::Params,
                s,
                "params",
            ),
            section_button(
                format!("{} ({})", t!("api.headers"), r.headers.len()),
                ReqSection::Headers,
                s,
                "headers",
            ),
            section_button(t!("api.body").to_string(), ReqSection::Body, s, "body"),
            section_button(t!("api.auth").to_string(), ReqSection::Auth, s, "auth"),
            section_button(
                t!("api.settings").to_string(),
                ReqSection::Settings,
                s,
                "settings",
            ),
            section_button(t!("api.tests").to_string(), ReqSection::Tests, s, "tests"),
        ])
        .spacing(4.0),
    );
    rows.push(request_part(api, tab));
    rows.push(separator());
    rows.push(response_part(tab));
    scroll(column(rows).spacing(8.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rows_of_the_tree_have_their_actions_in_a_context_menu() {
        use crate::api::model::Folder;
        let mut api = Api::load();
        let mut request = crate::api::model::Request::new("users");
        request.id = 3;
        let rid = request.id;
        api.collections = vec![Collection {
            id: 1,
            name: "Shop".into(),
            items: vec![
                Item::Folder(Folder {
                    id: 2,
                    name: "Admin".into(),
                    items: vec![],
                }),
                Item::Request(request),
            ],
        }];
        api.open.insert(1);
        let json = serde_json::to_value(panel(&api)).unwrap().to_string();
        let menu_of = |id: &str| -> Vec<String> {
            let node: serde_json::Value = serde_json::from_str(&json).unwrap();
            fn find(v: &serde_json::Value, id: &str) -> Option<serde_json::Value> {
                if v["id"] == id {
                    return Some(v["menu"].clone());
                }
                match v {
                    serde_json::Value::Object(m) => m.values().find_map(|v| find(v, id)),
                    serde_json::Value::Array(l) => l.iter().find_map(|v| find(v, id)),
                    _ => None,
                }
            }
            find(&node, id)
                .and_then(|m| m.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .map(|i| i["action"].as_str().unwrap_or_default().to_string())
                .collect()
        };
        let collection = menu_of("c:1:0");
        assert!(
            collection.contains(&"run".to_string()) && collection.contains(&"export".to_string())
        );
        let folder = menu_of("f:1:2");
        assert!(folder.contains(&"new_in".to_string()) && !folder.contains(&"run".to_string()));
        let request = menu_of(&format!("r:1:{rid}"));
        assert!(
            request.contains(&"duplicate".to_string()) && request.contains(&"delete".to_string())
        );
        assert!(!request.contains(&"new_in".to_string()));
        // No more buttons for what is picked.
        assert!(!json.contains("\"action\":\"rename\",\"label\""));
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(size(12), "12 B");
        assert_eq!(size(2048), "2.0 KB");
        assert_eq!(size(3 * 1024 * 1024), "3.0 MB");
    }

    /// The `enabled` of the button with this action, in a drawn node.
    fn enabled_of(node: &Node, action: &str) -> Option<bool> {
        fn find(v: &serde_json::Value, action: &str) -> Option<bool> {
            if v["action"] == action {
                return Some(v.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true));
            }
            match v {
                serde_json::Value::Object(m) => m.values().find_map(|c| find(c, action)),
                serde_json::Value::Array(a) => a.iter().find_map(|c| find(c, action)),
                _ => None,
            }
        }
        find(&serde_json::to_value(node).unwrap(), action)
    }

    #[test]
    fn a_request_without_an_address_cannot_be_sent_kept_or_copied() {
        let api = Api::load();
        let mut blank = ReqTab::new(crate::api::model::Request::new(""), None);
        let page = tab(&api, &blank);
        for action in ["send", "save", "copy_curl"] {
            assert_eq!(enabled_of(&page, action), Some(false), "{action}");
        }
        blank.request.url = "https://example.com".into();
        let page = tab(&api, &blank);
        for action in ["send", "save", "copy_curl"] {
            assert_eq!(enabled_of(&page, action), Some(true), "{action}");
        }
    }

    #[test]
    fn the_tools_of_the_panel_are_pictos_with_a_tooltip() {
        let api = Api::load();
        let json = serde_json::to_string(&panel(&api)).unwrap();
        for icon in [
            "plus",
            "folder-plus",
            "download",
            "terminal",
            "sliders-horizontal",
        ] {
            assert!(json.contains(&format!("\"icon\":\"{icon}\"")), "{icon}");
        }
        // No pseudo-picto of characters is left.
        assert!(!json.contains("\"label\":\"▤\"") && !json.contains("\"label\":\"↓\""));
    }
}
