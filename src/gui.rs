use crate::{
    apply::ApplyPlan,
    backend::{Candidate, State, Store},
    backend::{discover_hosts, host_flakes},
    repository::Repository,
};
use adw::prelude::*;
use gtk::{gio, glib};
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    fs,
    io::Write,
    path::PathBuf,
    process::Command,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

type Discovery = (u64, Result<(PathBuf, Vec<String>), String>);

const APP_ID: &str = "io.github.oddship.NixOSUpdates";

pub fn notify_background(state: &std::path::Path, candidate: &Candidate) {
    let app = gio::Application::new(Some(APP_ID), gio::ApplicationFlags::NON_UNIQUE);
    if app.register(None::<&gio::Cancellable>).is_ok() {
        notify_result(&app, state, candidate);
        if let Ok(connection) = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>)
        {
            let _ = connection.flush_sync(None::<&gio::Cancellable>);
        }
    }
}

fn notify_result(app: &gio::Application, state: &std::path::Path, candidate: &Candidate) {
    let body = match candidate.state {
        State::UpdatesFound => "Input updates are available. Open the app to prepare them.",
        State::Ready => "Your candidate is ready to review.",
        _ => return,
    };
    // Persist an announcement claim so reopening a window or polling cannot repeat it.
    let claim = (|| -> anyhow::Result<()> {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let directory = state.join("notifications");
        fs::create_dir_all(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join(format!("{}-{:?}", candidate.id, candidate.state)))?;
        file.sync_all()?;
        fs::File::open(directory)?.sync_all()?;
        Ok(())
    })();
    if claim.is_ok() {
        let notification = gio::Notification::new("NixOS Update Manager");
        notification.set_body(Some(body));
        notification.set_default_action("app.open");
        app.send_notification(Some("update-result"), &notification);
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Settings {
    path: PathBuf,
    host: String,
}

pub fn run(state: PathBuf) {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| {
        // Inherit libadwaita's light/dark palette; avoid a separate hard-coded theme.
        let css = gtk::CssProvider::new();
        css.load_from_data(".review-output, .review-output text { background-color: transparent; color: @window_fg_color; } .step-current { color: @accent_color; font-weight: bold; } .step-complete { font-weight: bold; }");
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
    });
    let open = gio::SimpleAction::new("open", None);
    open.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| app.activate()
    ));
    app.add_action(&open);
    app.connect_activate(move |app| window(app, state.clone()));
    app.run_with_args::<&str>(&[]);
}

fn window(app: &adw::Application, state: PathBuf) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("NixOS Update Manager")
        .default_width(720)
        .default_height(700)
        .build();
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let assistant = gtk::Button::with_label("Settings");
    assistant.set_tooltip_text(Some("Assistant settings"));
    assistant.set_accessible_role(gtk::AccessibleRole::Button);
    header.pack_end(&assistant);
    outer.append(&header);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 20);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.set_margin_top(12);
    content.set_margin_bottom(24);
    let setup = adw::PreferencesGroup::new();
    setup.set_title("Configuration");
    let path_label = adw::ActionRow::builder()
        .title("Choose your configuration")
        .subtitle("A local Git folder containing flake.nix")
        .build();
    let folder = gtk::Button::with_label("Choose folder…");
    folder.set_valign(gtk::Align::Center);
    path_label.add_suffix(&folder);
    setup.add(&path_label);
    let host_row = adw::ActionRow::builder().title("Host").build();
    let hosts = gtk::DropDown::from_strings(&[]);
    hosts.set_valign(gtk::Align::Center);
    hosts.set_sensitive(false);
    hosts.set_tooltip_text(Some("NixOS host configuration"));
    host_row.add_suffix(&hosts);
    setup.add(&host_row);
    content.append(&setup);

    let card = gtk::Box::new(gtk::Orientation::Vertical, 16);
    card.add_css_class("card");
    let card_content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    card_content.set_margin_start(20);
    card_content.set_margin_end(20);
    card_content.set_margin_top(20);
    card_content.set_margin_bottom(20);
    let steps = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let stages: Vec<gtk::Label> = ["1  Check", "2  Prepare", "3  Apply"]
        .iter()
        .map(|text| {
            let label = gtk::Label::new(Some(text));
            label.set_hexpand(true);
            label.add_css_class("dim-label");
            steps.append(&label);
            label
        })
        .collect();
    card_content.append(&steps);
    let heading = gtk::Label::new(Some("Start with your configuration"));
    heading.add_css_class("title-2");
    heading.set_xalign(0.0);
    heading.set_wrap(true);
    card_content.append(&heading);
    let status = gtk::Label::new(Some(
        "Choose a folder above. We’ll find the host configurations inside it.",
    ));
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.add_css_class("dim-label");
    card_content.append(&status);
    let progress = gtk::ProgressBar::new();
    progress.set_visible(false);
    progress.set_pulse_step(0.12);
    card_content.append(&progress);
    let summary = gtk::Label::new(None);
    summary.set_xalign(0.0);
    summary.set_wrap(true);
    summary.set_selectable(true);
    summary.set_visible(false);
    card_content.append(&summary);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let check = gtk::Button::with_label("Check for updates");
    check.add_css_class("suggested-action");
    check.set_sensitive(false);
    let prepare = gtk::Button::with_label("Prepare update");
    prepare.add_css_class("suggested-action");
    let cancel = gtk::Button::with_label("Cancel");
    let review = gtk::Button::with_label("Refresh review");
    let apply = gtk::Button::with_label("Apply update…");
    apply.add_css_class("suggested-action");
    let commit = gtk::Button::with_label("Retry commit");
    commit.add_css_class("suggested-action");
    for button in [&prepare, &apply, &commit, &review, &check, &cancel] {
        actions.append(button);
    }
    let explain = gtk::Button::with_label("Explain this update");
    explain.set_visible(false);
    explain.set_sensitive(false);
    actions.append(&explain);
    for button in [&prepare, &apply, &commit, &review, &cancel] {
        button.set_visible(false);
        button.set_sensitive(false);
    }
    card_content.append(&actions);
    card.append(&card_content);
    content.append(&card);

    let (inputs_group, inputs_list) = change_list("Changes by input", 180);
    content.append(&inputs_group);
    let (packages_group, packages_list) = change_list("Other packages and system components", 180);
    content.append(&packages_group);
    let changes = gtk::Expander::builder().label("Raw Nix review").build();
    let details = gtk::TextView::new();
    details.set_editable(false);
    details.set_cursor_visible(false);
    details.set_wrap_mode(gtk::WrapMode::WordChar);
    details.set_monospace(true);
    details.set_top_margin(12);
    details.set_bottom_margin(12);
    details.set_left_margin(12);
    details.set_right_margin(12);
    details.add_css_class("review-output");
    let diff_scroll = gtk::ScrolledWindow::builder()
        .child(&details)
        .min_content_height(160)
        .max_content_height(260)
        .propagate_natural_height(true)
        .build();
    changes.set_child(Some(&diff_scroll));
    changes.set_visible(false);
    content.append(&changes);
    let diagnostics = gtk::Expander::builder().label("Technical details").build();
    let technical = gtk::Label::new(None);
    technical.set_xalign(0.0);
    technical.set_wrap(true);
    technical.set_selectable(true);
    technical.add_css_class("monospace");
    technical.set_margin_top(12);
    diagnostics.set_child(Some(&technical));
    diagnostics.set_visible(false);
    content.append(&diagnostics);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&content)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    outer.append(&scroll);
    window.set_content(Some(&outer));
    let candidate: Rc<RefCell<Option<Candidate>>> = Rc::new(RefCell::new(None));
    let assistant_settings = Rc::new(RefCell::new(PiSettings::load(&state)));
    let explaining = Rc::new(Cell::new(false));
    assistant.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[strong]
        state,
        #[strong]
        assistant_settings,
        move |_| assistant_settings_dialog(&window, &state, &assistant_settings)
    ));
    explain.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[strong]
        state,
        #[strong]
        candidate,
        #[strong]
        assistant_settings,
        #[strong]
        explaining,
        move |_| {
            if let Some(candidate) = candidate.borrow().as_ref()
                && assistant_settings.borrow().connected()
                && !explaining.replace(true)
            {
                explanation_dialog(&window, &state, candidate, &assistant_settings, &explaining);
            }
        }
    ));
    glib::timeout_add_local(
        Duration::from_millis(150),
        glib::clone!(
            #[weak]
            progress,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                if progress.is_visible() {
                    progress.pulse();
                }
                glib::ControlFlow::Continue
            }
        ),
    );

    let selected: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let (tx, rx) = mpsc::channel::<Discovery>();
    let discovery = Arc::new(AtomicU64::new(0));
    let discovered = Rc::new(Cell::new(0_u64));
    folder.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[strong]
        tx,
        #[strong]
        selected,
        #[weak]
        hosts,
        #[strong]
        discovery,
        move |_| {
            let dialog = gtk::FileDialog::builder()
                .title("Choose NixOS configuration folder")
                .build();
            let tx = tx.clone();
            let discovery = discovery.clone();
            let selected = selected.clone();
            let hosts = hosts.clone();
            dialog.select_folder(Some(&window), None::<&gio::Cancellable>, move |result| {
                if let Ok(file) = result
                    && let Some(path) = file.path()
                {
                    *selected.borrow_mut() = None;
                    hosts.set_model(Some(&gtk::StringList::new(&[])));
                    discover(path, tx, discovery);
                }
            });
        }
    ));
    if let Ok(bytes) = fs::read(state.join("settings.json"))
        && let Ok(saved) = serde_json::from_slice::<Settings>(&bytes)
    {
        discover(saved.path, tx, discovery.clone());
    }
    glib::timeout_add_local(
        Duration::from_millis(200),
        glib::clone!(
            #[weak]
            hosts,
            #[weak]
            check,
            #[weak]
            status,
            #[weak]
            path_label,
            #[strong]
            selected,
            #[strong]
            state,
            #[strong]
            discovery,
            #[strong]
            discovered,
            #[weak]
            heading,
            #[weak]
            progress,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                if let Ok((request, result)) = rx.try_recv() {
                    if request != discovery.load(Ordering::SeqCst) {
                        return glib::ControlFlow::Continue;
                    }
                    discovered.set(request);
                    progress.set_visible(false);
                    match result {
                        Ok((path, names)) => {
                            hosts.set_model(Some(&gtk::StringList::new(
                                &names.iter().map(String::as_str).collect::<Vec<_>>(),
                            )));
                            let saved_host = fs::read(state.join("settings.json"))
                                .ok()
                                .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
                                .filter(|saved| saved.path == path)
                                .map(|saved| saved.host);
                            hosts.set_selected(
                                preferred_host(&names, saved_host.as_deref(), &glib::host_name())
                                    .map(|index| index as u32)
                                    .unwrap_or(gtk::INVALID_LIST_POSITION),
                            );
                            hosts.set_sensitive(true);
                            check.set_sensitive(!names.is_empty());
                            path_label.set_title(
                                path.file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or("Configuration"),
                            );
                            path_label.set_subtitle(&path.display().to_string());
                            *selected.borrow_mut() = Some(path);
                            status.set_text(if names.is_empty() {
                                "No NixOS hosts found in this folder."
                            } else {
                                "Check for new input revisions. Your system stays unchanged."
                            });
                        }
                        Err(error) => {
                            heading.set_text("Couldn’t read this configuration");
                            status.set_text(&error);
                        }
                    }
                }
                glib::ControlFlow::Continue
            }
        ),
    );

    check.connect_clicked(glib::clone!(
        #[weak]
        hosts,
        #[weak]
        status,
        #[strong]
        selected,
        #[strong]
        state,
        move |_| {
            let Some(path) = selected.borrow().clone() else {
                return;
            };
            let Some(host) = hosts.selected_item().and_downcast::<gtk::StringObject>() else {
                return;
            };
            let settings = Settings {
                path: path.clone(),
                host: host.string().to_string(),
            };
            let save = (|| -> anyhow::Result<()> {
                fs::create_dir_all(&state)?;
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
                let mut saved = tempfile::NamedTempFile::new_in(&state)?;
                saved.write_all(&serde_json::to_vec_pretty(&settings)?)?;
                saved.as_file().sync_all()?;
                saved.persist(state.join("settings.json"))?;
                fs::File::open(&state)?.sync_all()?;
                Ok(())
            })();
            if let Err(error) = save {
                status.set_text(&error.to_string());
                return;
            }
            match job(
                &state,
                "check",
                &[
                    path.to_string_lossy().into_owned(),
                    "--host".into(),
                    settings.host,
                ],
            ) {
                Ok(()) => status.set_text("Checking inputs in an isolated snapshot…"),
                Err(error) => status.set_text(&error),
            }
        }
    ));
    prepare.connect_clicked(glib::clone!(
        #[weak]
        status,
        #[strong]
        state,
        #[strong]
        candidate,
        move |_| {
            let Some(c) = candidate.borrow().clone() else {
                return;
            };
            match job(&state, "prepare", &[c.id]) {
                Ok(()) => status.set_text(
                    "Preparing the candidate. You can close this window and return later.",
                ),
                Err(error) => status.set_text(&error),
            }
        }
    ));
    apply.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[weak]
        app,
        #[weak]
        status,
        #[strong]
        state,
        #[strong]
        candidate,
        move |_| {
            let Some(c) = candidate.borrow().clone() else {
                return;
            };
            status.set_text("Validating the prepared update…");
            let id = c.id;
            let worker_state = state.clone();
            let worker_id = id.clone();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let result =
                    Store::open(&worker_state).and_then(|store| store.apply_plan(&worker_id));
                let _ = tx.send(result.map_err(|e| format!("{e:#}")));
            });
            glib::timeout_add_local(
                Duration::from_millis(100),
                glib::clone!(
                    #[weak]
                    window,
                    #[weak]
                    app,
                    #[weak]
                    status,
                    #[strong]
                    state,
                    #[upgrade_or]
                    glib::ControlFlow::Break,
                    move || match rx.try_recv() {
                        Ok(Ok(plan)) => {
                            confirm_apply(&window, &app, &status, state.clone(), id.clone(), plan);
                            glib::ControlFlow::Break
                        }
                        Ok(Err(error)) => {
                            status.set_text(&error);
                            glib::ControlFlow::Break
                        }
                        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                        Err(_) => {
                            status.set_text("Update validation worker stopped.");
                            glib::ControlFlow::Break
                        }
                    }
                ),
            );
        }
    ));
    commit.connect_clicked(glib::clone!(
        #[weak]
        app,
        #[weak]
        status,
        #[strong]
        state,
        #[strong]
        candidate,
        move |_| {
            if let Some(c) = candidate.borrow().as_ref() {
                run_application(&app, &status, state.clone(), c.id.clone(), None);
            }
        }
    ));
    let mut last = String::new();
    review.connect_clicked(glib::clone!(
        #[weak]
        status,
        #[strong]
        state,
        #[strong]
        candidate,
        move |_| {
            if let Some(c) = candidate.borrow().as_ref() {
                match job(&state, "review", std::slice::from_ref(&c.id)) {
                    Ok(()) => {
                        status.set_text("Refreshing the runtime review of the prepared output…")
                    }
                    Err(error) => status.set_text(&error),
                }
            }
        }
    ));
    cancel.connect_clicked(glib::clone!(
        #[weak]
        status,
        #[strong]
        state,
        #[strong]
        candidate,
        move |_| {
            if let Some(c) = candidate.borrow().as_ref() {
                match Store::request_cancel(&state, &c.id) {
                    Ok(true) => {
                        status.set_text("Cancellation requested. Waiting for the worker to stop…")
                    }
                    Ok(false) => status.set_text("The operation has already finished."),
                    Err(error) => status.set_text(&error.to_string()),
                }
            }
        }
    ));
    glib::timeout_add_local(
        Duration::from_secs(1),
        glib::clone!(
            #[weak]
            status,
            #[weak]
            heading,
            #[weak]
            progress,
            #[weak]
            summary,
            #[weak]
            inputs_group,
            #[weak]
            inputs_list,
            #[weak]
            packages_group,
            #[weak]
            packages_list,
            #[weak]
            changes,
            #[weak]
            diagnostics,
            #[weak]
            technical,
            #[strong]
            stages,
            #[weak]
            details,
            #[weak]
            prepare,
            #[weak]
            check,
            #[weak]
            cancel,
            #[weak]
            review,
            #[weak]
            apply,
            #[weak]
            commit,
            #[weak]
            explain,
            #[strong]
            assistant_settings,
            #[strong]
            explaining,
            #[weak]
            folder,
            #[weak]
            hosts,
            #[weak]
            app,
            #[strong]
            state,
            #[strong]
            candidate,
            #[strong]
            selected,
            #[strong]
            discovery,
            #[strong]
            discovered,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                if discovery.load(Ordering::SeqCst) != discovered.get() {
                    explain.set_visible(false);
                    heading.set_text("Finding host configurations");
                    status.set_text("Reading your flake. Your system stays unchanged.");
                    progress.set_visible(true);
                    check.set_sensitive(false);
                    hosts.set_sensitive(false);
                    for button in [&prepare, &apply, &commit, &review, &cancel] {
                        button.set_visible(false);
                    }
                    summary.set_visible(false);
                    inputs_group.set_visible(false);
                    packages_group.set_visible(false);
                    changes.set_visible(false);
                    diagnostics.set_visible(false);
                    last.clear();
                    return glib::ControlFlow::Continue;
                }
                // A live worker keeps the lock. Only abandoned records reconcile.
                let busy = Store::open(&state).is_err();
                let host = hosts
                    .selected_item()
                    .and_downcast::<gtk::StringObject>()
                    .map(|h| h.string().to_string());
                let current_path = selected.borrow().clone();
                let latest = current_path
                    .as_deref()
                    .zip(host.as_deref())
                    .and_then(|(path, host)| Store::latest_for(&state, path, host));
                check.set_sensitive(!busy && current_path.is_some() && host.is_some());
                if let Some(c) = latest {
                    let signature = serde_json::to_string(&c).unwrap_or_default();
                    if signature != last {
                        last = signature;
                        let presentation = present(&c.state);
                        heading.set_text(presentation.0);
                        status.set_text(presentation.1);
                        progress.set_visible(is_working(&c.state));
                        for (index, stage) in stages.iter().enumerate() {
                            stage.remove_css_class("step-current");
                            stage.remove_css_class("step-complete");
                            stage.remove_css_class("dim-label");
                            stage.add_css_class(if index < presentation.2 {
                                "step-complete"
                            } else if index == presentation.2 {
                                "step-current"
                            } else {
                                "dim-label"
                            });
                        }
                        let mut text = String::new();
                        clear_rows(&inputs_list);
                        let package_changes =
                            crate::review::package_changes(c.closure_diff.as_deref().unwrap_or(""));
                        let listed_count = package_changes.len();
                        let (mut grouped, other) = crate::review::group_packages(
                            package_changes,
                            &c.installed_input_packages,
                        );
                        for name in &c.changed_inputs {
                            let packages = grouped.remove(name).unwrap_or_default();
                            let subtitle = if c.closure_diff.is_none() {
                                "Prepare to see package version changes".to_owned()
                            } else if packages.is_empty() {
                                "No direct package mapping · see other changes below".to_owned()
                            } else {
                                format!(
                                    "{} package {}",
                                    packages.len(),
                                    if packages.len() == 1 {
                                        "change"
                                    } else {
                                        "changes"
                                    }
                                )
                            };
                            let row = adw::ExpanderRow::builder()
                                .title(name)
                                .subtitle(&subtitle)
                                .expanded(!packages.is_empty())
                                .build();
                            row.set_use_markup(false);
                            row.set_enable_expansion(!packages.is_empty());
                            for package in packages {
                                row.add_row(&change_row(&package.name, &package.description));
                            }
                            if let Some(change) =
                                c.input_changes.iter().find(|change| &change.name == name)
                            {
                                row.set_tooltip_text(Some(&format!(
                                    "Revision {} → {}",
                                    change.current.as_deref().unwrap_or("unavailable"),
                                    change.next.as_deref().unwrap_or("unavailable")
                                )));
                            }
                            inputs_list.append(&row);
                        }
                        inputs_group.set_visible(!c.changed_inputs.is_empty());
                        clear_rows(&packages_list);
                        for change in &other {
                            packages_list.append(&change_row(&change.name, &change.description));
                        }
                        let package_changes = other;
                        packages_list.set_visible(!package_changes.is_empty());
                        packages_group.set_description(Some(if c.closure_diff.is_none() {
                            "Prepare the update to see the versions that will change."
                        } else if package_changes.is_empty() && c.closure_diff.as_deref().is_some_and(|diff| !diff.trim().is_empty()) {
                            "See the raw Nix review below for changes that could not be listed."
                        } else if package_changes.is_empty() {
                            "No package version or size changes reported by Nix."
                        } else {
                            "Shared, removed, overlay-provided, or otherwise unmapped changes. Unchanged versions are omitted."
                        }));
                        packages_group.set_visible(
                            c.closure_diff.is_none()
                                || !package_changes.is_empty()
                                || (listed_count == 0
                                    && c.closure_diff
                                        .as_deref()
                                        .is_some_and(|diff| !diff.trim().is_empty())),
                        );
                        if c.fingerprint
                            .status
                            .split('\0')
                            .any(|entry| !entry.is_empty() && !entry.starts_with("?? "))
                        {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str("Your tracked local edits are included in this update.");
                        }
                        if let Some(application) = &c.application
                            && let Some(commit) = &application.commit
                        {
                            text.push_str(&format!(
                                "\nLock update committed: {}",
                                &commit[..commit.len().min(12)]
                            ));
                        }
                        summary.set_text(text.trim());
                        summary.set_visible(!text.trim().is_empty());
                        details
                            .buffer()
                            .set_text(c.closure_diff.as_deref().unwrap_or(""));
                        changes.set_visible(c.closure_diff.is_some());
                        changes.set_expanded(false);
                        let mut technical_text = format!("Host: {}\nCandidate: {}", c.host, c.id);
                        for input in &c.input_changes {
                            technical_text.push_str(&format!(
                                "\n\n{} revision: {} → {}",
                                input.name,
                                input.current.as_deref().unwrap_or("unavailable"),
                                input.next.as_deref().unwrap_or("unavailable")
                            ));
                        }
                        if let Some(error) = &c.attribution_error {
                            technical_text.push_str(&format!("\n\nPackage mapping: {error}"));
                        }
                        if !c.skipped_inputs.is_empty() {
                            technical_text.push_str(&format!(
                                "\nExcluded inputs: {}",
                                c.skipped_inputs.join(", ")
                            ));
                        }
                        if !c.fingerprint.status.is_empty() {
                            technical_text.push_str(&format!(
                                "\n\nGit status:\n{}",
                                c.fingerprint.status.replace('\0', "\n")
                            ));
                        }
                        if let Some(system) = &c.prepared_system {
                            technical_text
                                .push_str(&format!("\n\nPrepared system: {}", system.display()));
                        }
                        if let Some(application) = &c.application
                            && let Some(actual) = &application.actual
                        {
                            technical_text.push_str(&format!(
                                "\nRunning system: {}\nSystem profile: {}",
                                actual.running.display(),
                                actual.profile.display()
                            ));
                        }
                        if let Some(error) = &c.error {
                            technical_text.push_str(&format!("\n\n{}", error));
                        }
                        technical.set_text(&technical_text);
                        diagnostics.set_visible(true);
                        diagnostics.set_expanded(c.error.is_some());
                        prepare.set_sensitive(
                            matches!(c.state, State::UpdatesFound | State::InputOnly)
                                || (matches!(c.state, State::Failed | State::Cancelled)
                                    && c.candidate_drv.is_some()),
                        );
                        cancel.set_sensitive(matches!(c.state, State::Checking | State::Preparing));
                        notify_result(app.upcast_ref(), &state, &c);
                        *candidate.borrow_mut() = Some(c);
                    }
                    let c = candidate.borrow();
                    if let Some(c) = c.as_ref() {
                        let working = is_working(&c.state);
                        explain.set_visible(c.closure_diff.is_some() && !working);
                        explain.set_sensitive(
                            assistant_settings.borrow().connected() && !explaining.get() && !busy,
                        );
                        explain.set_tooltip_text(Some(
                            if assistant_settings.borrow().connected() {
                                "Explain the prepared package changes using your model"
                            } else {
                                "Test the assistant connection in Settings to enable explanations"
                            },
                        ));
                        folder.set_sensitive(!busy);
                        hosts.set_sensitive(!busy);
                        cancel.set_visible(matches!(
                            c.state,
                            State::Checking | State::Preparing | State::Reviewing
                        ));
                        apply.set_visible(matches!(c.state, State::Ready));
                        commit.set_visible(matches!(c.state, State::CommitNeedsAttention));
                        review.set_visible(matches!(c.state, State::ReviewStale));
                        prepare.set_visible(
                            matches!(c.state, State::UpdatesFound | State::InputOnly)
                                || (matches!(c.state, State::Failed | State::Cancelled)
                                    && c.candidate_drv.is_some()),
                        );
                        check.set_visible(!working);
                        if prepare.is_visible()
                            || apply.is_visible()
                            || commit.is_visible()
                            || review.is_visible()
                        {
                            check.set_label("Check again");
                            check.remove_css_class("suggested-action");
                        } else {
                            check.set_label("Check for updates");
                            check.add_css_class("suggested-action");
                        }
                        apply.set_sensitive(!busy && matches!(c.state, State::Ready));
                        commit
                            .set_sensitive(!busy && matches!(c.state, State::CommitNeedsAttention));
                        cancel.set_sensitive(matches!(
                            c.state,
                            State::Checking | State::Preparing | State::Reviewing
                        ));
                        review.set_sensitive(
                            !busy
                                && c.prepared_system.is_some()
                                && matches!(
                                    c.state,
                                    State::Ready
                                        | State::ReviewStale
                                        | State::Failed
                                        | State::Cancelled
                                ),
                        );
                        prepare.set_sensitive(
                            !busy
                                && (matches!(c.state, State::UpdatesFound | State::InputOnly)
                                    || (matches!(c.state, State::Failed | State::Cancelled)
                                        && c.candidate_drv.is_some())),
                        );
                    }
                } else {
                    explain.set_visible(false);
                    let context = format!("empty:{current_path:?}:{host:?}:{busy}");
                    if context != last {
                        heading.set_text(if current_path.is_none() {
                            "Start with your configuration"
                        } else if host.is_none() {
                            "Choose a host"
                        } else {
                            "Ready to check"
                        });
                        for stage in &stages {
                            stage.remove_css_class("step-current");
                            stage.remove_css_class("step-complete");
                            stage.add_css_class("dim-label");
                        }
                        if current_path.is_some() {
                            status.set_text(if busy {
                                "Another updater operation is running."
                            } else if host.is_none() {
                                "Select a NixOS host configuration above."
                            } else {
                                "Check for new input revisions. Your system stays unchanged."
                            });
                        }
                        last = context;
                    }
                    progress.set_visible(false);
                    summary.set_visible(false);
                    inputs_group.set_visible(false);
                    packages_group.set_visible(false);
                    changes.set_visible(false);
                    diagnostics.set_visible(false);
                    folder.set_sensitive(!busy);
                    hosts.set_sensitive(!busy && current_path.is_some());
                    for button in [&prepare, &apply, &commit, &review, &cancel] {
                        button.set_visible(false);
                    }
                    check.set_visible(true);
                    check.set_label("Check for updates");
                    check.add_css_class("suggested-action");
                    prepare.set_sensitive(false);
                    cancel.set_sensitive(false);
                    review.set_sensitive(false);
                    apply.set_sensitive(false);
                    commit.set_sensitive(false);
                    details.buffer().set_text("");
                    *candidate.borrow_mut() = None;
                }
                glib::ControlFlow::Continue
            }
        ),
    );
    window.present();
}

fn change_list(title: &str, minimum_height: i32) -> (adw::PreferencesGroup, gtk::ListBox) {
    let group = adw::PreferencesGroup::builder()
        .title(title)
        .visible(false)
        .build();
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(minimum_height)
        .max_content_height(420)
        .propagate_natural_height(true)
        .build();
    group.add(&scroll);
    (group, list)
}

fn clear_rows(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

fn change_row(name: &str, description: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(name)
        .subtitle(description)
        .build();
    row.set_use_markup(false);
    row.set_title_lines(2);
    row.set_subtitle_lines(3);
    row.set_tooltip_text(Some(&format!("{name}\n{description}")));
    row
}

fn discover(path: PathBuf, tx: mpsc::Sender<Discovery>, generation: Arc<AtomicU64>) {
    let request = generation.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<_> {
            let names = host_flakes(&path);
            if !names.is_empty() {
                return Ok((path.canonicalize()?, names));
            }
            let repo = Repository::open(&path)?;
            let temp = tempfile::tempdir()?;
            let flake = repo.snapshot(&temp.path().join("source"), &repo.fingerprint()?)?;
            Ok((repo.root.join(&repo.flake_dir), discover_hosts(&flake)?))
        })();
        let _ = tx.send((request, result.map_err(|e| format!("{e:#}"))));
    });
}

fn job(state: &std::path::Path, operation: &str, args: &[String]) -> Result<(), String> {
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    // Re-enter the installed wrapper, preserving its pinned Nix/Git/GApps paths.
    let executable = current
        .parent()
        .ok_or("executable has no parent")?
        .join("nixos-update-manager");
    let mut command = Command::new("systemd-run");
    command
        .args([
            "--user",
            "--collect",
            "--quiet",
            "--unit",
            &format!("nixos-updates-{operation}"),
        ])
        .arg("--property=Nice=10");
    if let Ok(cores) = std::env::var("NIXOS_UPDATES_BUILD_CORES")
        && cores.parse::<u32>().is_ok_and(|cores| cores > 0)
    {
        command.arg(format!("--setenv=NIXOS_UPDATES_BUILD_CORES={cores}"));
    }
    let result = command
        .arg(executable)
        .arg("--notify")
        .arg("--state-dir")
        .arg(state)
        .arg(operation)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if result.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&result.stderr).into_owned())
    }
}

fn confirm_apply(
    window: &adw::ApplicationWindow,
    app: &adw::Application,
    status: &gtk::Label,
    state: PathBuf,
    id: String,
    plan: ApplyPlan,
) {
    let dialog = adw::AlertDialog::builder()
        .heading("Apply this prepared update?")
        .body(format!(
            "Activate the system you just reviewed and update flake.lock. Services may restart.{}",
            if plan.includes_local_edits {
                " Your tracked local edits are included."
            } else {
                ""
            }
        ))
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("apply", "Apply")]);
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    let message = gtk::Entry::builder()
        .text(&plan.commit_message)
        .hexpand(true)
        .build();
    if plan.can_commit {
        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let label = gtk::Label::new(Some("Commit message (flake.lock only)"));
        label.set_xalign(0.0);
        box_.append(&label);
        box_.append(&message);
        dialog.set_extra_child(Some(&box_));
        dialog.add_response("commit", "Apply and commit");
        dialog.set_response_appearance("commit", adw::ResponseAppearance::Suggested);
    } else {
        dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
    }
    dialog.choose(
        Some(window),
        None::<&gio::Cancellable>,
        glib::clone!(
            #[weak]
            app,
            #[weak]
            status,
            move |response| {
                if response == "apply" || response == "commit" {
                    run_application(
                        &app,
                        &status,
                        state,
                        id,
                        Some((response == "commit", message.text().to_string())),
                    );
                }
            }
        ),
    );
}

// Keep GApplication alive even if the window closes during authentication or activation.
// The root helper has its own lease/deadline; persisted records recover interrupted workers.
fn run_application(
    app: &adw::Application,
    status: &gtk::Label,
    state: PathBuf,
    id: String,
    apply: Option<(bool, String)>,
) {
    let hold = app.hold();
    let (tx, rx) = mpsc::channel();
    status.set_text(if apply.is_some() {
        "Waiting for authentication…"
    } else {
        "Retrying the lock-only commit…"
    });
    std::thread::spawn(move || {
        let result = Store::open(&state).and_then(|store| match apply {
            Some((commit, message)) => store.apply(&id, commit, Some(message)),
            None => store.commit(&id),
        });
        let _ = tx.send(result.map_err(|e| format!("{e:#}")));
    });
    let status = status.downgrade();
    glib::timeout_add_local(Duration::from_millis(200), move || {
        let _keep_alive = &hold;
        match rx.try_recv() {
            Ok(result) => {
                if let Some(status) = status.upgrade() {
                    match result {
                        Ok(c) => status.set_text(c.error.as_deref().unwrap_or(
                            "Operation completed. See the recorded system and commit below.",
                        )),
                        Err(error) => status.set_text(&error),
                    }
                }
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(_) => glib::ControlFlow::Break,
        }
    });
}

fn preferred_host(names: &[String], saved: Option<&str>, hostname: &str) -> Option<usize> {
    saved
        .and_then(|host| names.iter().position(|name| name == host))
        .or_else(|| names.iter().position(|name| name == hostname))
        .or_else(|| {
            names
                .iter()
                .position(|name| name == hostname.split('.').next().unwrap_or(hostname))
        })
        .or_else(|| (names.len() == 1).then_some(0))
}

fn is_working(state: &State) -> bool {
    matches!(
        state,
        State::Checking
            | State::Preparing
            | State::Reviewing
            | State::AwaitingAuthentication
            | State::Applying
            | State::Committing
    )
}

fn present(state: &State) -> (&'static str, &'static str, usize) {
    match state {
        State::Checking => (
            "Checking for updates",
            "Looking for new input revisions. Your system stays unchanged.",
            0,
        ),
        State::UpToDate => (
            "You’re up to date",
            "No updates found for the selected inputs.",
            0,
        ),
        State::InputOnly => (
            "New input revisions",
            "These input changes leave the system derivation unchanged. Prepare them to review before applying.",
            1,
        ),
        State::UpdatesFound => (
            "Updates available",
            "Prepare the update to download, build, and compare it with your running system.",
            1,
        ),
        State::Preparing => (
            "Preparing your update",
            "Downloading and building. You can close this window; we’ll notify you when it’s ready.",
            1,
        ),
        State::Reviewing => (
            "Refreshing the review",
            "Comparing the prepared update with your running system.",
            1,
        ),
        State::Ready => (
            "Ready to apply",
            "Review the changes below. Apply will ask for system authentication.",
            2,
        ),
        State::ReviewStale => (
            "Refresh the review",
            "Your running system has changed. Compare it again before applying.",
            1,
        ),
        State::Stale => (
            "Check again",
            "Your configuration has changed since this update was prepared.",
            0,
        ),
        State::Failed => (
            "Couldn’t finish the update",
            "Open Technical details for the error, then check again or retry preparation.",
            1,
        ),
        State::Cancelled => (
            "Update cancelled",
            "You can check again or retry preparation.",
            1,
        ),
        State::AwaitingAuthentication => (
            "Authentication needed",
            "Use the system prompt to approve this update.",
            2,
        ),
        State::Applying => (
            "Applying your update",
            "Activating the reviewed system. Services may restart.",
            2,
        ),
        State::Applied => (
            "Update applied",
            "Your system is running the prepared update.",
            3,
        ),
        State::ApplyNeedsAttention => (
            "Review the apply result",
            "The apply operation did not complete cleanly. Check the recorded system paths and error below before proceeding.",
            2,
        ),
        State::Committing => (
            "Saving the lock update",
            "Committing flake.lock. Your other changes stay separate.",
            2,
        ),
        State::Committed => (
            "Update complete",
            "Your system is updated and flake.lock is committed.",
            3,
        ),
        State::CommitNeedsAttention => (
            "System updated; commit unfinished",
            "Your update is applied. Resolve the Git error below, then retry the commit without applying again.",
            3,
        ),
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct PiSettings {
    model: String,
    verified_program: Option<PathBuf>,
    verified_model: Option<String>,
    #[serde(skip)]
    generation: u64,
}

impl PiSettings {
    fn load(state: &std::path::Path) -> Self {
        fs::read(state.join("assistant.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn connected_for(&self, program: &std::path::Path) -> bool {
        self.verified_program.as_deref() == Some(program)
            && self.verified_model.as_deref() == Some(self.model.as_str())
    }

    fn connected(&self) -> bool {
        crate::pi::program().is_some_and(|program| self.connected_for(&program))
    }

    fn invalidate(&mut self) -> u64 {
        self.verified_program = None;
        self.verified_model = None;
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    fn verify(&mut self, generation: u64, program: PathBuf, model: &str) -> bool {
        if generation != self.generation || model != self.model {
            return false;
        }
        self.verified_program = Some(program);
        self.verified_model = Some(model.to_owned());
        true
    }

    fn save(&self, state: &std::path::Path) -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(state)?;
        fs::set_permissions(state, fs::Permissions::from_mode(0o700))?;
        let mut file = tempfile::NamedTempFile::new_in(state)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.as_file().sync_all()?;
        file.persist(state.join("assistant.json"))?;
        fs::File::open(state)?.sync_all()?;
        Ok(())
    }
}

fn assistant_settings_dialog(
    window: &adw::ApplicationWindow,
    state: &std::path::Path,
    settings: &Rc<RefCell<PiSettings>>,
) {
    let dialog = adw::Dialog::builder()
        .title("Assistant settings")
        .content_width(520)
        .content_height(400)
        .build();
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
    layout.append(&adw::HeaderBar::new());
    let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.set_margin_top(16);
    body.set_margin_bottom(24);
    let status = gtk::Label::new(Some(if settings.borrow().connected() {
        "Last connection test passed. Explanations are enabled."
    } else {
        "Test your saved Pi account and model to enable explanations."
    }));
    status.set_wrap(true);
    status.set_xalign(0.0);
    body.append(&status);
    let test = gtk::Button::with_label("Test connection");
    test.add_css_class("suggested-action");
    test.set_halign(gtk::Align::Start);
    body.append(&test);
    let note = gtk::Label::new(Some("Sends a short test message to your selected model."));
    note.set_wrap(true);
    note.set_xalign(0.0);
    note.add_css_class("dim-label");
    body.append(&note);
    let details = gtk::Expander::builder()
        .label("Connection details")
        .visible(false)
        .build();
    let error = gtk::Label::new(None);
    error.set_wrap(true);
    error.set_selectable(true);
    error.set_xalign(0.0);
    details.set_child(Some(&error));
    body.append(&details);

    let options = gtk::Expander::builder().label("Account and model").build();
    let options_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let model = adw::EntryRow::builder()
        .title("Model override (optional, provider/model)")
        .text(&settings.borrow().model)
        .build();
    options_box.append(&model);
    let manage = gtk::Button::with_label("Open Pi");
    manage.set_halign(gtk::Align::Start);
    options_box.append(&manage);
    let help = gtk::Label::new(Some(
        "Use /login to change accounts or /model to select a model. Leave the override empty to use Pi’s saved model.",
    ));
    help.set_wrap(true);
    help.set_xalign(0.0);
    help.add_css_class("dim-label");
    options_box.append(&help);
    options.set_child(Some(&options_box));
    body.append(&options);
    let data = gtk::Expander::builder()
        .label("Data sent for explanations")
        .build();
    let privacy = gtk::Label::new(Some(
        "Input names and up to 200 package changes. No repository files, paths, hashes or error logs. Pi tools and project resources are disabled.",
    ));
    privacy.set_wrap(true);
    privacy.set_xalign(0.0);
    data.set_child(Some(&privacy));
    body.append(&data);

    let state = state.to_owned();
    model.connect_changed(glib::clone!(
        #[strong]
        settings,
        #[strong]
        state,
        #[weak]
        status,
        move |model| {
            let mut settings = settings.borrow_mut();
            settings.model = model.text().trim().to_owned();
            settings.invalidate();
            status.set_text(if settings.save(&state).is_ok() {
                "Model changed. Test the connection to enable explanations."
            } else {
                "Couldn’t save assistant settings. Test the connection again."
            });
        }
    ));
    if let Some(program) = crate::pi::program() {
        manage.connect_clicked(glib::clone!(
            #[strong]
            program,
            #[weak]
            status,
            #[weak]
            details,
            #[weak]
            error,
            move |_| {
                if let Err(failure) = crate::pi::open_login(&program) {
                    status.set_text("Couldn’t open Pi.");
                    error.set_text(&format!("{failure:#}"));
                    details.set_visible(true);
                }
            }
        ));
        test.connect_clicked(glib::clone!(
            #[strong] settings,
            #[strong] state,
            #[strong] program,
            #[weak] status,
            #[weak] details,
            #[weak] error,
            #[weak] model,
            move |test| {
                let generation = settings.borrow_mut().invalidate();
                let model_name = settings.borrow().model.clone();
                let _ = settings.borrow().save(&state);
                test.set_sensitive(false);
                test.set_label("Testing…");
                model.set_sensitive(false);
                status.set_text("Testing the connection…");
                details.set_visible(false);
                let (tx, rx) = mpsc::channel();
                let requested_program = program.clone();
                let requested_model = model_name.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(crate::pi::test_connection(&requested_program, &requested_model).map_err(|failure| format!("{failure:#}")));
                });
                // Keep the result consumer alive even if Settings is closed.
                // The bounded request can then unlock the contextual action.
                glib::timeout_add_local(Duration::from_millis(100), glib::clone!(
                    #[strong] settings,
                    #[strong] state,
                    #[strong] program,
                    #[strong] status,
                    #[strong] details,
                    #[strong] error,
                    #[strong] model,
                    #[strong] test,
                    move || {
                        let outcome = match rx.try_recv() {
                            Ok(outcome) => outcome,
                            Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                            Err(_) => Err("The connection test stopped before completing.".into()),
                        };
                        test.set_sensitive(true);
                        test.set_label("Test connection");
                        model.set_sensitive(true);
                        let mut settings = settings.borrow_mut();
                        if settings.generation != generation {
                            status.set_text("Settings changed. Test the connection again.");
                            return glib::ControlFlow::Break;
                        }
                        match outcome {
                            Ok(_) => {
                                settings.verify(generation, program.clone(), &model_name);
                                if settings.save(&state).is_ok() {
                                    status.set_text("Connection test passed. Explanations are enabled.");
                                } else {
                                    settings.invalidate();
                                    status.set_text("Connection worked, but settings couldn’t be saved. Try again.");
                                }
                            }
                            Err(failure) => {
                                let _ = settings.save(&state);
                                status.set_text("Connection failed. Check your account and model in Pi, then try again.");
                                error.set_text(&failure);
                                details.set_visible(true);
                            }
                        }
                        glib::ControlFlow::Break
                    }
                ));
            }
        ));
    } else {
        manage.set_sensitive(false);
        test.set_sensitive(false);
        status
            .set_text("Pi is unavailable. Use the default Nix package or install pi-coding-agent.");
    }
    layout.append(
        &gtk::ScrolledWindow::builder()
            .child(&body)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build(),
    );
    dialog.set_child(Some(&layout));
    dialog.present(Some(window));
}

fn explanation_dialog(
    window: &adw::ApplicationWindow,
    state: &std::path::Path,
    candidate: &Candidate,
    settings: &Rc<RefCell<PiSettings>>,
    explaining: &Rc<Cell<bool>>,
) {
    let Some(program) = crate::pi::program() else {
        explaining.set(false);
        return;
    };
    let model = settings.borrow().model.clone();
    let generation = settings.borrow().generation;
    let dialog = adw::Dialog::builder()
        .title("Update explanation")
        .content_width(560)
        .content_height(440)
        .build();
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
    layout.append(&adw::HeaderBar::new());
    let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.set_margin_top(16);
    body.set_margin_bottom(24);
    let result = gtk::Label::new(Some("Explaining the prepared changes…"));
    result.set_wrap(true);
    result.set_selectable(true);
    result.set_xalign(0.0);
    body.append(&result);
    let details = gtk::Expander::builder()
        .label("Technical details")
        .visible(false)
        .build();
    let error = gtk::Label::new(None);
    error.set_wrap(true);
    error.set_selectable(true);
    error.set_xalign(0.0);
    details.set_child(Some(&error));
    body.append(&details);
    let privacy = gtk::Label::new(Some(
        "Sends input names and up to 200 package changes to your selected model.",
    ));
    privacy.set_wrap(true);
    privacy.set_xalign(0.0);
    privacy.add_css_class("dim-label");
    body.append(&privacy);
    layout.append(
        &gtk::ScrolledWindow::builder()
            .child(&body)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build(),
    );
    dialog.set_child(Some(&layout));
    dialog.present(Some(window));
    let candidate = candidate.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(
            crate::pi::explain(&program, &model, &candidate)
                .map_err(|failure| format!("{failure:#}")),
        );
    });
    let state = state.to_owned();
    glib::timeout_add_local(
        Duration::from_millis(100),
        glib::clone!(
            #[strong]
            explaining,
            #[strong]
            settings,
            #[strong]
            result,
            #[strong]
            error,
            #[strong]
            details,
            move || {
                let outcome = match rx.try_recv() {
                    Ok(outcome) => outcome,
                    Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                    Err(_) => Err("The explanation stopped before completing.".into()),
                };
                explaining.set(false);
                match outcome {
                    Ok(answer) => result.set_text(&answer),
                    Err(failure) => {
                        let mut settings = settings.borrow_mut();
                        if generation == settings.generation {
                            settings.invalidate();
                            let _ = settings.save(&state);
                        }
                        result.set_text(
                            "Couldn’t get an explanation. Check the connection in Settings.",
                        );
                        error.set_text(&failure);
                        details.set_visible(true);
                    }
                }
                glib::ControlFlow::Break
            }
        ),
    );
}

#[cfg(test)]
mod ui_tests {
    use super::*;

    #[test]
    fn verified_connection_persists_and_old_tests_cannot_enable_a_changed_model() {
        let temp = tempfile::tempdir().unwrap();
        let program = std::path::Path::new("/nix/store/test-pi/bin/pi");
        let mut settings = PiSettings::default();
        assert!(!settings.connected_for(program));
        let first = settings.invalidate();
        assert!(settings.verify(first, program.to_owned(), ""));
        settings.save(temp.path()).unwrap();
        let loaded = PiSettings::load(temp.path());
        assert!(loaded.connected_for(program));
        assert!(!loaded.connected_for(std::path::Path::new("/nix/store/other-pi/bin/pi")));
        settings.model = "provider/other-model".into();
        let second = settings.invalidate();
        assert!(!settings.connected_for(program));
        assert!(!settings.verify(first, program.to_owned(), ""));
        assert!(settings.verify(second, program.to_owned(), "provider/other-model"));
        let third = settings.invalidate();
        assert!(!settings.verify(second, program.to_owned(), "provider/other-model"));
        assert!(settings.verify(third, program.to_owned(), "provider/other-model"));
    }

    #[test]
    fn host_selection_respects_saved_choice_then_machine_and_requires_ambiguous_choice() {
        let names = vec!["desktop".into(), "laptop".into()];
        assert_eq!(preferred_host(&names, Some("laptop"), "desktop"), Some(1));
        assert_eq!(preferred_host(&names, Some("removed"), "desktop"), Some(0));
        assert_eq!(preferred_host(&names, None, "laptop.example.net"), Some(1));
        assert_eq!(preferred_host(&names, None, "unknown"), None);
        assert_eq!(preferred_host(&[], None, "desktop"), None);
        assert_eq!(
            preferred_host(&["only-host".into()], None, "unknown"),
            Some(0)
        );
    }
}
