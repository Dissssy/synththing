//! The identity's recovery and rotation, in Preferences > Library
//! (docs/SERVER.md, Authorities): an email attached at the authority (the
//! official server) for recovery, a new key in place of the old one, and
//! recovering with the email when the key's lost. The records of each
//! change are kept beside the key and shown to every server the app talks
//! to (once a session), which moves what the old keys had to the new one.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui;

use super::library::host;
use super::App;
use crate::library::identity::{self, Identity};
use crate::library::rotation::{EmailConfirm, EmailRequest, IdentityStatus, Rotation};
use crate::library::{self, client};

enum Event {
    Status(Result<IdentityStatus, String>),
    EmailSent(Result<Vec<String>, String>),
    EmailDone(Result<bool, String>),
    /// A new key in place (rotated or recovered), and the records leading
    /// to it.
    NewKey(Box<Result<(Identity, Vec<Rotation>), String>>, &'static str),
    RecoverSent(Result<(), String>),
    /// A deletion asked for (or confirmed), or a restore: what happened.
    Deleted(Result<client::Deletion, String>),
    Restored(Result<u64, String>),
    Applied(String, Result<u64, String>),
}

/// What's being changed.
enum Flow {
    /// Attach (no current), change, or take off (`removing`) the email.
    Email {
        removing: bool,
        address: String,
        current: String,
        /// Which addresses got a code ("new", "current"), once sent.
        sent: Option<Vec<String>>,
        code: String,
        current_code: String,
    },
    Rotate,
    /// Recover, to `new`: the address, then the code it got.
    Recover { address: String, new: Identity, sent: bool, code: String },
    /// Delete the identity's data from `server` (the attached email, and
    /// the code it got, where it has one), or (`restore`) undo that.
    Delete { server: String, restore: bool, address: String, sent: bool, code: String, done: Option<String> },
}

struct FlowState {
    flow: Flow,
    busy: bool,
    error: Option<String>,
}

pub struct IdentityState {
    events: (Sender<Event>, Receiver<Event>),
    /// What the authority knows of the identity (asked once, and after a
    /// change).
    status: Option<Result<IdentityStatus, String>>,
    asked: bool,
    flow: Option<FlowState>,
    /// Servers shown the rotation records this session.
    applied: HashSet<String>,
}

impl Default for IdentityState {
    fn default() -> Self {
        Self { events: mpsc::channel(), status: None, asked: false, flow: None, applied: HashSet::new() }
    }
}

impl IdentityState {
    pub fn open(&self) -> bool {
        self.flow.is_some()
    }
}

impl App {
    fn spawn_identity(&self, job: impl FnOnce() -> Event + Send + 'static) {
        let tx = self.identity.events.0.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job());
        });
    }

    /// The authority (the official server) and its key, pinned or (the
    /// first time) asked for.
    fn authority(&self) -> (String, Option<String>) {
        let server = library::official_url();
        let key = self.config.library.pinned.get(&server).cloned();
        (server, key)
    }

    /// Show `server` how the identity came to be its key, once a session.
    pub(super) fn apply_rotations(&mut self, server: &str) {
        let chain = identity::load_rotations();
        if chain.is_empty() || !self.identity.applied.insert(server.to_string()) {
            return;
        }
        let server = server.to_string();
        self.spawn_identity(move || Event::Applied(server.clone(), client::apply(&server, &chain)));
    }

    pub(super) fn poll_identity(&mut self) {
        while let Ok(event) = self.identity.events.1.try_recv() {
            let flow = self.identity.flow.as_mut();
            match event {
                Event::Status(status) => self.identity.status = Some(status),
                Event::Applied(server, result) => match result {
                    Ok(0) => {}
                    Ok(n) => log::info!("{server} took {n} rotation record(s)"),
                    Err(e) => log::warn!("{server} didn't take the rotation records: {e}"),
                },
                Event::EmailSent(result) => {
                    if let Some(FlowState { flow: Flow::Email { sent, .. }, busy, error }) = flow {
                        *busy = false;
                        match result {
                            Ok(to) => *sent = Some(to),
                            Err(e) => *error = Some(e),
                        }
                    }
                }
                Event::EmailDone(result) => match result {
                    Ok(attached) => {
                        self.identity.flow = None;
                        self.identity.status = Some(Ok(IdentityStatus { email: attached, mail: true }));
                        self.library_identity_message(if attached {
                            "Email attached: it can recover your identity now."
                        } else {
                            "Email taken off."
                        });
                    }
                    Err(e) => {
                        if let Some(state) = flow {
                            state.busy = false;
                            state.error = Some(e);
                        }
                    }
                },
                Event::Deleted(result) => {
                    if let Some(FlowState { flow: Flow::Delete { sent, done, server, .. }, busy, error }) = flow {
                        *busy = false;
                        match result {
                            Ok(client::Deletion { deleted: Some(until), .. }) => {
                                let date = crate::song_info::format_length((until - unix_now()).max(0) as f64);
                                *done = Some(format!(
                                    "Deleted from {}: it all goes for good in {date}, unless you restore it before then.",
                                    host(server)
                                ));
                            }
                            Ok(_) => *sent = true,
                            Err(e) => *error = Some(e),
                        }
                    }
                }
                Event::Restored(result) => {
                    if let Some(FlowState { flow: Flow::Delete { done, server, .. }, busy, error }) = flow {
                        *busy = false;
                        match result {
                            Ok(n) => {
                                *done = Some(format!(
                                    "Restored on {}: {n} script{} back, with everything else.",
                                    host(server),
                                    if n == 1 { "" } else { "s" }
                                ))
                            }
                            Err(e) => *error = Some(e),
                        }
                    }
                }
                Event::RecoverSent(result) => {
                    if let Some(FlowState { flow: Flow::Recover { sent, .. }, busy, error }) = flow {
                        *busy = false;
                        match result {
                            Ok(()) => *sent = true,
                            Err(e) => *error = Some(e),
                        }
                    }
                }
                Event::NewKey(result, how) => match *result {
                    Ok((new, chain)) => {
                        let saved = new.save().and_then(|()| identity::save_rotations(&chain));
                        match saved {
                            Ok(()) => {
                                log::info!("the library identity is #{} now ({how})", new.id());
                                self.library_identity_message(&format!(
                                    "Your identity's key is #{} now. Export it, to keep it safe.",
                                    new.id()
                                ));
                                self.set_library_identity(new);
                                self.identity.flow = None;
                                self.identity.applied.clear();
                                self.identity.status = None;
                                self.identity.asked = false;
                            }
                            Err(e) => {
                                if let Some(state) = flow {
                                    state.busy = false;
                                    state.error = Some(format!("It worked, but the new key couldn't be saved: {e}"));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        if let Some(state) = flow {
                            state.busy = false;
                            state.error = Some(e);
                        }
                    }
                },
            }
        }
    }

    /// Preferences > Library, under the identity: its email, a new key,
    /// recovery.
    pub(super) fn recovery_ui(&mut self, ui: &mut egui::Ui, current: Option<&Identity>) {
        let (authority, key) = self.authority();
        if let Some(identity) = current
            && !self.identity.asked
        {
            self.identity.asked = true;
            let (identity, authority, key) = (identity.clone(), authority.clone(), key.clone());
            self.spawn_identity(move || {
                let key = match key {
                    Some(key) => Ok(key),
                    None => client::info(&authority).map(|i| i.key),
                };
                Event::Status(key.and_then(|key| client::identity_status(&authority, &key, &identity)))
            });
        }
        let mut open: Option<Flow> = None;
        if current.is_some() {
            ui.horizontal_wrapped(|ui| {
                ui.label("Recovery email:");
                match &self.identity.status {
                    None => {
                        ui.spinner();
                    }
                    Some(Err(e)) => {
                        ui.weak(format!("couldn't ask {} ({e})", host(&authority)));
                    }
                    Some(Ok(status)) if !status.mail => {
                        ui.weak(format!("{} doesn't send email yet", host(&authority)));
                    }
                    Some(Ok(status)) if status.email => {
                        ui.label("attached");
                        if ui.small_button("Change...").clicked() {
                            open = Some(Flow::Email {
                                removing: false,
                                address: String::new(),
                                current: String::new(),
                                sent: None,
                                code: String::new(),
                                current_code: String::new(),
                            });
                        }
                        if ui.small_button("Take off...").clicked() {
                            open = Some(Flow::Email {
                                removing: true,
                                address: String::new(),
                                current: String::new(),
                                sent: None,
                                code: String::new(),
                                current_code: String::new(),
                            });
                        }
                    }
                    Some(Ok(_)) => {
                        ui.weak("none");
                        if ui.small_button("Add...").clicked() {
                            open = Some(Flow::Email {
                                removing: false,
                                address: String::new(),
                                current: String::new(),
                                sent: None,
                                code: String::new(),
                                current_code: String::new(),
                            });
                        }
                    }
                }
                super::info_icon(ui).on_hover_text(format!(
                    "An email attached to your identity on {} gets it back if you lose your key: a code is sent \
                     to it, and the identity moves to a new key. Only a hash of the address is kept, it's never \
                     shown or shared, and changing or taking it off needs a code sent to it too.",
                    host(&authority)
                ));
            });
        }
        ui.horizontal(|ui| {
            if current.is_some()
                && ui
                    .button("New key...")
                    .on_hover_text("Move your identity to a new key (if the old one might have got out)")
                    .clicked()
            {
                open = Some(Flow::Rotate);
            }
            if ui
                .button("Recover...")
                .on_hover_text("Lost your key? Get your identity back with the email attached to it")
                .clicked()
            {
                match Identity::generate() {
                    Ok(new) => open = Some(Flow::Recover { address: String::new(), new, sent: false, code: String::new() }),
                    Err(e) => self.library_identity_message(&format!("Couldn't make a key: {e}")),
                }
            }
        });
        if current.is_some() {
            ui.horizontal(|ui| {
                for (restore, label, hover) in [
                    (false, "Delete my data...", "Have a server delete what it has of yours: your scripts, encores, email"),
                    (true, "Restore my data...", "Undo a deletion, within 30 days of it"),
                ] {
                    if ui.button(label).on_hover_text(hover).clicked() {
                        open = Some(Flow::Delete {
                            server: authority.clone(),
                            restore,
                            address: String::new(),
                            sent: false,
                            code: String::new(),
                            done: None,
                        });
                    }
                }
            });
        }
        if let Some(flow) = open {
            self.identity.flow = Some(FlowState { flow, busy: false, error: None });
        }
    }

    /// The window for an email change, a new key, or a recovery.
    pub(super) fn identity_flow_ui(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.identity.flow.take() else { return };
        let (authority, key) = self.authority();
        let current = self.library_identity();
        let mut close = false;
        let mut go = false;
        let response = egui::Modal::new(egui::Id::new("identity_flow")).show(ctx, |ui| {
            ui.set_width(440.0);
            match &mut state.flow {
                Flow::Email { removing, address, current, sent, code, current_code } => {
                    let changing = matches!(&self.identity.status, Some(Ok(s)) if s.email) && !*removing;
                    ui.heading(if *removing {
                        "Take the email off"
                    } else if changing {
                        "Change the email"
                    } else {
                        "Add a recovery email"
                    });
                    match sent {
                        None => {
                            if *removing || changing {
                                ui.label("The address attached now (a code goes to it):");
                                ui.add(egui::TextEdit::singleline(current).desired_width(f32::INFINITY));
                            }
                            if !*removing {
                                ui.label(if changing { "The new address:" } else { "Your email address:" });
                                ui.add(egui::TextEdit::singleline(address).desired_width(f32::INFINITY));
                                ui.weak(format!(
                                    "{} keeps only a hash of it, to recognise it when you recover. It's never shown \
                                     or shared, and only ever gets codes you ask for.",
                                    host(&authority)
                                ));
                            }
                        }
                        Some(to) => {
                            if to.iter().any(|s| s == "current") {
                                ui.label("The code sent to the address attached now:");
                                ui.add(egui::TextEdit::singleline(current_code).hint_text("ABCD-EFGH").desired_width(160.0));
                            }
                            if to.iter().any(|s| s == "new") {
                                ui.label(format!("The code sent to {}:", address.trim()));
                                ui.add(egui::TextEdit::singleline(code).hint_text("ABCD-EFGH").desired_width(160.0));
                            }
                            ui.weak("Codes work for 30 minutes. No email? Check the spam folder.");
                        }
                    }
                }
                Flow::Rotate => {
                    ui.heading("Move to a new key");
                    ui.label(format!(
                        "A new key replaces #{}: your scripts, encores and admin rights move to it, on {} now and \
                         on other servers the next time the app talks to them, and the old key stops working \
                         wherever that's happened. Do this if the old key might have got out.",
                        current.as_ref().map(Identity::id).unwrap_or_default(),
                        host(&authority)
                    ));
                    ui.weak("Export the new one afterwards: an export of the old key won't work any more.");
                }
                Flow::Delete { server, restore, address, sent, code, done } => {
                    ui.heading(if *restore { "Restore your data" } else { "Delete your data" });
                    if let Some(done) = done {
                        ui.label(done.as_str());
                    } else {
                        if !*sent {
                            ui.horizontal(|ui| {
                                ui.label("On");
                                egui::ComboBox::from_id_salt("delete_server").selected_text(host(server)).show_ui(ui, |ui| {
                                    for s in self.config.library.enabled_servers() {
                                        let label = host(&s).to_string();
                                        ui.selectable_value(server, s, label);
                                    }
                                });
                            });
                        }
                        if *restore {
                            ui.label(format!(
                                "Puts back what {} was going to delete: your scripts, encores, admin rights. Within \
                                 30 days of deleting.",
                                host(server)
                            ));
                        } else if *sent {
                            ui.label("The code sent to your email:");
                            ui.add(egui::TextEdit::singleline(code).hint_text("ABCD-EFGH").desired_width(160.0));
                            ui.weak("It works for 30 minutes. No email? Check the spam folder.");
                        } else {
                            ui.label(format!(
                                "{} deletes what it has of yours: your scripts (their remixes then say they were \
                                 remixed from a deleted script), the encores you gave, your admin rights, your \
                                 email. It all goes from listings straight away, and for good after 30 days: until \
                                 then, Restore my data puts it back. Bans stay, and so do reports (without your key).",
                                host(server)
                            ));
                            if self.library_server_mail(server) {
                                ui.label("If your identity has an email attached here, type it (a code goes to it):");
                                ui.add(egui::TextEdit::singleline(address).desired_width(f32::INFINITY));
                            }
                        }
                    }
                }
                Flow::Recover { address, sent, code, .. } => {
                    ui.heading("Recover your identity");
                    if *sent {
                        ui.label(format!(
                            "If {} is attached to an identity, a code is on its way to it. Type it here:",
                            address.trim()
                        ));
                        ui.add(egui::TextEdit::singleline(code).hint_text("ABCD-EFGH").desired_width(160.0));
                        ui.weak("It works for 30 minutes. No email? Check the spam folder, or the address.");
                    } else {
                        ui.label("The email attached to the identity:");
                        ui.add(egui::TextEdit::singleline(address).desired_width(f32::INFINITY));
                        ui.weak(match &current {
                            Some(identity) => format!(
                                "The identity moves to a new key, made here, which replaces this app's #{} (export \
                                 that first if it's a different identity you want to keep).",
                                identity.id()
                            ),
                            None => "The identity moves to a new key, made here.".to_string(),
                        });
                    }
                }
            }
            if let Some(error) = &state.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let finished = matches!(&state.flow, Flow::Delete { done: Some(_), .. });
                let label = match &state.flow {
                    Flow::Delete { restore: true, .. } => "Restore",
                    Flow::Delete { sent: true, .. } => "Confirm",
                    Flow::Delete { .. } => "Delete",
                    Flow::Email { sent: None, .. } => "Send code",
                    Flow::Email { .. } => "Confirm",
                    Flow::Rotate => "Make a new key",
                    Flow::Recover { sent: false, .. } => "Send code",
                    Flow::Recover { .. } => "Recover",
                };
                if !finished && ui.add_enabled(!state.busy, egui::Button::new(label)).clicked() {
                    go = true;
                }
                if ui.button(if finished { "Close" } else { "Cancel" }).clicked() {
                    close = true;
                }
                if state.busy {
                    ui.spinner();
                }
            });
        });
        if go && let Flow::Delete { server, restore, address, sent, code, .. } = &state.flow {
            state.error = None;
            match (current.clone(), self.config.library.pinned.get(server).cloned()) {
                (Some(identity), Some(server_key)) => {
                    state.busy = true;
                    let (server, address, code, restore, sent) =
                        (server.clone(), address.trim().to_string(), code.trim().to_string(), *restore, *sent);
                    self.spawn_identity(move || {
                        if restore {
                            Event::Restored(client::restore_identity(&server, &server_key, &identity))
                        } else if sent {
                            Event::Deleted(client::delete_confirm(&server, &server_key, &identity, &code))
                        } else {
                            let address = Some(address.as_str()).filter(|a| !a.is_empty());
                            Event::Deleted(client::delete_identity(&server, &server_key, &identity, address))
                        }
                    });
                }
                (None, _) => state.error = Some("There's no identity here.".into()),
                (_, None) => state.error = Some("That server hasn't been reached yet: open the Script Library first.".into()),
            }
        } else if go {
            state.error = None;
            state.busy = true;
            let key = key.unwrap_or_default();
            if key.is_empty() {
                state.busy = false;
                state.error = Some(format!("{} hasn't been reached yet: open the Script Library first.", host(&authority)));
            } else {
                self.identity_step(&mut state, authority, key, current);
            }
        }
        if !(close || (response.should_close() && !state.busy)) {
            self.identity.flow = Some(state);
        }
    }

    /// The next step of a flow, on a thread (all but Delete, done above).
    fn identity_step(&mut self, state: &mut FlowState, authority: String, key: String, current: Option<Identity>) {
        match &state.flow {
            Flow::Email { removing, address, current: current_address, sent, code, current_code } => {
                let Some(identity) = current else { return };
                if sent.is_none() {
                    let ask = EmailRequest {
                        address: (!*removing).then(|| address.trim().to_string()),
                        current: Some(current_address.trim().to_string()).filter(|c| !c.is_empty()),
                    };
                    if ask.address.as_deref().is_some_and(|a| !library::rotation::is_email(a)) {
                        state.busy = false;
                        state.error = Some("That isn't an email address.".into());
                        return;
                    }
                    self.spawn_identity(move || Event::EmailSent(client::email(&authority, &key, &identity, &ask)));
                } else {
                    let confirm = EmailConfirm {
                        code: Some(code.trim().to_string()).filter(|c| !c.is_empty()),
                        current_code: Some(current_code.trim().to_string()).filter(|c| !c.is_empty()),
                    };
                    self.spawn_identity(move || Event::EmailDone(client::email_confirm(&authority, &key, &identity, &confirm)));
                }
            }
            Flow::Rotate => {
                let Some(old) = current else { return };
                self.spawn_identity(move || {
                    let result = Identity::generate().and_then(|new| {
                        let record = client::rotate(&authority, &key, &old, &new)?;
                        let chain = client::lineage(&authority, &key, &new.public()).unwrap_or_else(|_| {
                            let mut chain = identity::load_rotations();
                            chain.push(record);
                            chain
                        });
                        Ok((new, chain))
                    });
                    Event::NewKey(Box::new(result), "a new key")
                });
            }
            Flow::Delete { .. } => {}
            Flow::Recover { address, new, sent, code } => {
                let (address, new, code) = (address.trim().to_string(), new.clone(), code.trim().to_string());
                if !*sent {
                    if !library::rotation::is_email(&address) {
                        state.busy = false;
                        state.error = Some("That isn't an email address.".into());
                        return;
                    }
                    self.spawn_identity(move || Event::RecoverSent(client::recover(&authority, &key, &address, &new)));
                } else {
                    self.spawn_identity(move || {
                        let result = client::recover_confirm(&authority, &key, &new, &code).map(|record| {
                            let chain = client::lineage(&authority, &key, &new.public()).unwrap_or_else(|_| vec![record]);
                            (new, chain)
                        });
                        Event::NewKey(Box::new(result), "recovered by email")
                    });
                }
            }
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}
