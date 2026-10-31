//! The Android shell: storage folders, `Services` backed by the Java activity, the inbox,
//! working copies, finger gestures, the on-screen keyboard, the slide show's full screen and the
//! eframe runner.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use deckcraft_engine::Session;
use deckcraft_ui_egui::{Inbox, InboxEvent, InboxFile, Services, SlideApp};
use serde_json::json;
use winit::platform::android::activity::AndroidApp;

use crate::mailbox::{self, Message, Outcome};
use crate::workcopies::{Notice, State, WorkCopies};
use crate::{bridge, keys, touch::Touches};

/// The app's private folders (under `Context.getFilesDir()`, the same folders the Java side uses).
#[derive(Clone)]
struct Dirs {
    /// Settings (`ui.json`, `prefs.json`) and the working copies' index.
    profile: PathBuf,
    /// Messages from Java ([`mailbox`]).
    inbox: PathBuf,
    /// Files waiting for Java to save or print them.
    outbox: PathBuf,
    /// Working copies of presentations opened from storage ([`crate::workcopies`]).
    docs: PathBuf,
    /// The system clipboard's text, as the activity last saw it.
    clipboard: PathBuf,
    /// Its HTML, when it holds some.
    clipboard_html: PathBuf,
}

impl Dirs {
    fn new(files: &Path) -> Self {
        Self {
            profile: files.join("profile"),
            inbox: files.join("inbox"),
            outbox: files.join("outbox"),
            docs: files.join("docs"),
            clipboard: files.join("clipboard.txt"),
            clipboard_html: files.join("clipboard.html"),
        }
    }
}

pub fn run(android: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("DeckCraft"));
    std::panic::set_hook(Box::new(|info| log::error!("panic: {info}")));
    let files = android.internal_data_path().unwrap_or_else(|| PathBuf::from("/data/local/tmp/deckcraft"));
    let dirs = Dirs::new(&files);
    for d in [&dirs.profile, &dirs.inbox, &dirs.outbox, &dirs.docs] {
        if let Err(e) = std::fs::create_dir_all(d) {
            log::warn!("cannot create {}: {e}", d.display());
        }
    }
    // Files left in the outbox were handed to an earlier run's save picker or printer.
    clear_dir(&dirs.outbox);
    // SAFETY: called on the app thread before eframe or any worker of ours starts, so nothing in
    // this process reads the environment concurrently. The recovery folder and the user font
    // folder go by HOME.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("HOME", &files);
    }
    log::info!("DeckCraft {} for Android, data in {}", env!("CARGO_PKG_VERSION"), files.display());

    bridge::init(&android);
    let options = eframe::NativeOptions { android_app: Some(android), ..Default::default() };
    let result = eframe::run_native(
        "DeckCraft",
        options,
        Box::new(move |cc| {
            if let Some(rs) = &cc.wgpu_render_state {
                log::info!("wgpu backend {:?} on {}", rs.adapter.get_info().backend, rs.adapter.get_info().name);
            }
            Ok(Box::new(Shell::new(&cc.egui_ctx, dirs)))
        }),
    );
    if let Err(e) = result {
        log::error!("DeckCraft stopped: {e}");
    }
}

/// A file handed to the save picker, by ticket.
struct SaveJob {
    /// The name the app asked for.
    name: String,
    /// The presentation it is a copy of (the active one when the save started), once known.
    doc: Option<u64>,
}

#[derive(Default)]
struct Saves {
    next: u64,
    jobs: BTreeMap<u64, SaveJob>,
}

/// What egui-winit's own clipboard holds so a hardware keyboard's Ctrl+V reaches the app.
/// egui-winit can't read Android's clipboard: it turns Ctrl+V into a paste of its own copy, and
/// into nothing at all while that copy is empty. Seeded with this invisible character, every
/// Ctrl+V arrives as a paste, whose text the shell then takes from the system clipboard.
const PASTE_SEED: &str = "\u{2063}";

struct Shell {
    app: SlideApp,
    dirs: Dirs,
    /// Files the app asked for (or that arrived from another app), delivered to it.
    inbox: Inbox,
    touches: Touches,
    /// Pickers and saves started and not yet answered (logged when the app closes with any).
    waiting: Rc<Cell<u32>>,
    /// The keyboard as last asked of the activity.
    keyboard: bool,
    /// Where the last tap or press began, and whether File view was open then.
    press: Option<(egui::Pos2, bool)>,
    /// When the last tap ended (seconds, egui's clock): a text field that takes the keyboard's
    /// focus soon after (a double tap into a text box, a tap on the notes) shows the keyboard.
    tapped_at: Option<f64>,
    /// Messages for the user from the host (a picker that couldn't open).
    messages: Rc<RefCell<Vec<String>>>,
    /// The settings as last written.
    prefs_written: Option<String>,
    /// The window edges covered by system bars or the keyboard (pixels).
    insets: Option<egui::epaint::MarginF32>,
    /// Whether the system bars' icons were last set for a light app (dark icons).
    light_bars: Option<bool>,
    /// Whether the system bars were last hidden for a slide show.
    immersive: bool,
    copies: WorkCopies,
    saves: Rc<RefCell<Saves>>,
    /// The working copies open in the last frame, to notice one being closed.
    open_copies: Vec<String>,
    /// Whether egui-winit's own clipboard holds [`PASTE_SEED`] yet.
    seeded: bool,
}

impl Shell {
    fn new(ctx: &egui::Context, dirs: Dirs) -> Self {
        deckcraft_ui_egui::install_ime_pass_hooks(ctx);
        let inbox = Inbox::default();
        let waiting = Rc::new(Cell::new(0));
        let messages = Rc::new(RefCell::new(Vec::new()));
        let saves = Rc::new(RefCell::new(Saves::default()));
        let services = services(&dirs, inbox.clone(), waiting.clone(), messages.clone(), saves.clone());
        let mut app = SlideApp::new(Session::new(), services);
        let (ui, prefs) = deckcraft_ui_egui::settings::load(&dirs.profile);
        if let Some(ui) = ui {
            app.ui = ui;
        }
        if let Some(prefs) = prefs {
            app.session.prefs = prefs;
        }
        // Android may end the app whenever it is in the background: unsaved presentations come
        // back from the recovery folder on the next start.
        app.session.recovery_dir = deckcraft_engine::recovery::default_dir();
        if let Ok(v) = app.session.execute("file.recovery.open", &json!({}))
            && let Some(n) = v.get("opened").and_then(|o| o.as_array()).map(Vec::len).filter(|n| *n > 0)
        {
            app.notify(format!("Recovered {n} unsaved presentation{} from the last session", if n == 1 { "" } else { "s" }));
        }
        let mut copies = WorkCopies::load(dirs.docs.clone(), dirs.profile.join("workcopies.json"));
        let kept = copies.tidy();
        // A presentation whose changes its own file lacks (another app changed the file, or
        // writing failed) opens again, as it was left.
        for (path, state) in &kept {
            match app.open_path(path) {
                Ok(()) => app.notify(not_saved_message(path, *state)),
                Err(e) => log::warn!("reopening {path}: {e}"),
            }
        }
        let prefs_written = settings_json(&app);
        watch_inbox(dirs.inbox.clone(), ctx.clone());
        Self {
            app,
            dirs,
            inbox,
            touches: Touches::default(),
            waiting,
            keyboard: false,
            press: None,
            tapped_at: None,
            messages,
            prefs_written,
            insets: None,
            light_bars: None,
            immersive: false,
            copies,
            saves,
            open_copies: kept.into_iter().map(|(p, _)| p).collect(),
            seeded: false,
        }
    }

    /// Deliver what the Java side left in the inbox.
    fn read_inbox(&mut self, raw: &mut egui::RawInput) {
        for message in mailbox::drain(&self.dirs.inbox) {
            match message {
                Message::Opened { name, purpose, uri, id, bytes } if purpose == "open" => self.open_presentation(name, uri, id, bytes),
                Message::Opened { name, id, bytes, .. } => self.deliver(InboxEvent::File(InboxFile { id, name, bytes })),
                Message::PickerClosed { id, .. } => {
                    self.waiting.set(self.waiting.get().saturating_sub(1));
                    // Nothing more comes for this request (a file it delivered came first).
                    self.deliver(InboxEvent::Cancelled { id });
                }
                Message::Saved { ticket, requested, outcome } => {
                    self.waiting.set(self.waiting.get().saturating_sub(1));
                    let job = self.saves.borrow_mut().jobs.remove(&ticket);
                    if job.as_ref().is_some_and(|j| j.name != requested) {
                        log::warn!("save {ticket}: the answer names {requested}");
                    }
                    match outcome {
                        Outcome::Saved { name, uri } => {
                            log::info!("saved {requested} as {name}");
                            if mailbox::is_presentation_save(&requested)
                                && mailbox::is_presentation_save(&name)
                                && let Some(doc) = job.and_then(|j| j.doc)
                            {
                                self.adopt(doc, &requested, &name, &uri);
                            }
                            self.app.set_status(format!("Saved {name}"));
                        }
                        Outcome::Cancelled => {}
                        Outcome::Failed(reason) => self.app.notify(format!("Couldn't save {requested}: {reason}")),
                    }
                }
                Message::Synced { path, outcome } => match self.copies.answered(&path, outcome) {
                    Notice::None => {}
                    Notice::Conflict(_) => self.app.notify(not_saved_message(&path, State::Conflict)),
                    Notice::Failed(_, reason) => {
                        self.app.notify(format!("{} ({reason})", not_saved_message(&path, State::Failed)));
                    }
                },
                Message::Text(text) => raw.events.push(egui::Event::Text(text)),
                Message::Key { code, meta } => raw.events.extend(keys::press(code, meta)),
                Message::Insets { left, top, right, bottom } => {
                    log::info!("window insets: left {left} top {top} right {right} bottom {bottom}");
                    self.insets = Some(egui::epaint::MarginF32 { left, right, top, bottom });
                }
            }
        }
    }

    fn deliver(&self, event: InboxEvent) {
        self.inbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(event);
    }

    /// A presentation picked in File › Open, or opened or shared from another app. One the app
    /// may write back to (and that DeckCraft saves in full) opens from a working copy, so Save
    /// works as on the desktop; any other opens as an untitled copy, which Save asks where to keep.
    fn open_presentation(&mut self, name: String, uri: Option<String>, id: u64, bytes: Vec<u8>) {
        log::info!("received {name} ({} bytes, {})", bytes.len(), if uri.is_some() { "writable" } else { "read only" });
        // A file from another app has no request of its own: open it as one picked now.
        let id = if id == 0 { self.app.begin_external_file("open") } else { id };
        let Some(uri) = uri.filter(|_| mailbox::is_presentation_save(&name)) else {
            self.deliver(InboxEvent::File(InboxFile { id, name, bytes }));
            return;
        };
        self.deliver(InboxEvent::Cancelled { id });
        // Already open from the same file: show it rather than a second copy.
        if let Some(i) = self.app.session.documents().iter().position(|d| d.path.as_deref().is_some_and(|p| self.copies.uri(p) == Some(uri.as_str())))
        {
            self.app.session.set_active(i);
            self.app.close_backstage();
            return;
        }
        match self.copies.create(&name, &bytes, &uri) {
            Ok(path) => {
                let path = path.to_string_lossy().into_owned();
                if let Err(e) = self.app.open_path(&path) {
                    self.app.notify(format!("Couldn't open {name}: {e}"));
                }
                self.app.close_backstage();
            }
            Err(e) => {
                log::warn!("no working copy for {name}: {e}");
                let id = self.app.begin_external_file("open");
                self.deliver(InboxEvent::File(InboxFile { id, name, bytes }));
            }
        }
    }

    /// Save As put presentation `doc` in storage as `name` at `uri`: like Save As on the desktop,
    /// the presentation now has that file (a working copy linked to it), its title says so, and
    /// the next Save writes there. Saved in the format `name` says, written back at once when the
    /// user picked another type than the app offered (`requested`).
    fn adopt(&mut self, doc: u64, requested: &str, name: &str, uri: &str) {
        let Some(index) = self.app.session.documents().iter().position(|d| d.uid == doc) else { return };
        let path = match self.copies.new_path(name) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("no working copy for {name}: {e}");
                return;
            }
        };
        let old = self.app.session.documents().get(index).and_then(|d| d.path.clone());
        let active = self.app.session.active_index();
        self.app.session.set_active(index);
        let saved = self.app.session.execute("file.saveAs", &json!({"path": path.to_string_lossy()}));
        if let Some(active) = active {
            self.app.session.set_active(active);
        }
        match saved {
            Ok(_) => {
                let ext = |n: &str| n.rsplit('.').next().map(str::to_ascii_lowercase);
                if ext(requested) == ext(name) {
                    self.copies.link(&path, uri);
                } else {
                    self.copies.link_unsynced(&path, uri);
                }
                if let Some(old) = old.filter(|p| self.copies.contains(p)) {
                    self.copies.forget(&old);
                }
            }
            Err(e) => log::warn!("{name} saved, but the presentation keeps its old file: {e}"),
        }
    }

    /// Hand saved working copies to the activity, which writes them over their originals; note
    /// which presentation a save just started is a copy of; keep Recent to files this device can
    /// open again.
    fn after_app(&mut self) {
        let open: Vec<String> = self.app.session.documents().iter().filter_map(|d| d.path.clone()).collect();
        // A presentation closed while its own file lacks its changes: the user was told, so its
        // copy goes rather than coming back at every start.
        for closed in std::mem::take(&mut self.open_copies).into_iter().filter(|p| !open.contains(p)) {
            if matches!(self.copies.state(&closed), Some(State::Conflict | State::Failed)) {
                log::info!("working copies: {closed} closed without its changes reaching its file");
                self.copies.forget(&closed);
            }
        }
        self.open_copies = open.iter().filter(|p| self.copies.contains(p)).cloned().collect();
        for (path, uri) in self.copies.saved(&open) {
            if let Err(e) = bridge::write_back(&path, &uri) {
                log::warn!("write back {path}: {e}");
                let _ = self.copies.answered(&path, mailbox::Sync::Failed(e));
            }
        }
        let active = self.app.session.active().map(|d| d.uid);
        for job in self.saves.borrow_mut().jobs.values_mut() {
            if job.doc.is_none() {
                job.doc = active;
            }
        }
        // Working copies come and go; Android's own picker (File › Open) lists recent files.
        let docs = self.dirs.docs.to_string_lossy().into_owned();
        self.app.ui.recent.retain(|p| !p.starts_with(&docs));
    }

    /// Write the settings when they changed: Android may stop the app without warning.
    fn store_prefs(&mut self) {
        let json = settings_json(&self.app);
        if json.is_some() && json != self.prefs_written {
            deckcraft_ui_egui::settings::save(&self.dirs.profile, &self.app.ui, &self.app.session.prefs);
            self.prefs_written = json;
        }
    }

    /// Show the on-screen keyboard when a text field takes typing right after a tap; put it away
    /// when nothing takes typing.
    fn keyboard_follows(&mut self, ctx: &egui::Context, typing_area: Option<egui::Rect>, released: bool, now: f64) {
        if released && let Some((pos, from_file_view)) = self.press.take() {
            let tapped_field = typing_area.is_some_and(|area| area.expand(8.0).contains(pos));
            if !from_file_view || tapped_field {
                self.tapped_at = Some(now);
            }
        }
        match typing_area {
            None => {
                if self.keyboard {
                    self.keyboard = false;
                    if let Err(e) = bridge::keyboard(false) {
                        log::warn!("keyboard: {e}");
                    }
                }
            }
            Some(_) => {
                let recent = self.tapped_at.is_some_and(|t| now - t < 0.8);
                if recent && (!self.keyboard || released) {
                    self.keyboard = true;
                    self.tapped_at = None;
                    if let Err(e) = bridge::keyboard(true) {
                        log::warn!("keyboard: {e}");
                    }
                }
            }
        }
        if self.tapped_at.is_some_and(|t| now - t < 0.8) {
            // A field may take focus a frame or two after the tap (a double tap starts editing).
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    /// A slide show fills the whole screen: the status and navigation bars go while it runs.
    fn follow_show(&mut self) {
        let showing = self.app.show.is_some();
        if showing != self.immersive {
            self.immersive = showing;
            if let Err(e) = bridge::immersive(showing) {
                log::warn!("full screen: {e}");
            }
        }
    }
}

impl eframe::App for Shell {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.read_inbox(raw);
        // Keep the app clear of the status bar, the navigation bar, a camera cutout and the
        // on-screen keyboard: Android draws apps edge to edge, under all of them.
        if let Some(px) = self.insets {
            let ppp = ctx.pixels_per_point().max(0.1);
            let points = egui::epaint::MarginF32 { left: px.left / ppp, right: px.right / ppp, top: px.top / ppp, bottom: px.bottom / ppp };
            raw.safe_area_insets = Some(egui::SafeAreaInsets(points));
        }
        for event in &mut raw.events {
            match event {
                // The system Back button closes what's open (and ends a slide show), as Escape does.
                egui::Event::Key { key: key @ egui::Key::BrowserBack, .. } => *key = egui::Key::Escape,
                // Paste takes the system clipboard (another app's copy), when it has text.
                egui::Event::Paste(text) => match read_text(&self.dirs.clipboard) {
                    Some(system) => *text = system,
                    None if text.as_str() == PASTE_SEED => text.clear(),
                    None => {}
                },
                _ => {}
            }
        }
        // Ctrl+V with nothing on the clipboard pastes nothing.
        raw.events.retain(|e| !matches!(e, egui::Event::Paste(text) if text.is_empty()));
        if self.touches.process(raw) {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        self.app.raw_input_hook(ctx, raw);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let messages = std::mem::take(&mut *self.messages.borrow_mut());
        if let Some(last) = messages.into_iter().last() {
            self.app.notify(last);
        }
        self.app.logic(ctx);
        self.after_app();
        self.store_prefs();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let (pressed_at, released, now) =
            ui.ctx().input(|i| (i.pointer.any_pressed().then(|| i.pointer.press_origin()).flatten(), i.pointer.any_released(), i.time));
        if let Some(pos) = pressed_at {
            self.press = Some((pos, self.app.backstage.is_some()));
        }
        // Android draws the app under the status bar, the navigation bar and the keyboard: run it
        // in the part of the window they leave free (`content_rect`, from the insets), over the
        // app's own background. A slide show takes the whole screen.
        self.follow_show();
        let free = if self.immersive { ui.ctx().viewport_rect() } else { ui.ctx().content_rect() };
        let background = if self.immersive { egui::Color32::BLACK } else { ui.visuals().panel_fill };
        ui.painter().rect_filled(ui.ctx().viewport_rect(), 0.0, background);
        let light = !ui.visuals().dark_mode;
        if self.light_bars != Some(light) {
            self.light_bars = Some(light);
            if let Err(e) = bridge::light_system_bars(light) {
                log::warn!("system bars: {e}");
            }
        }
        ui.scope_builder(egui::UiBuilder::new().max_rect(free), |ui| {
            ui.set_clip_rect(free);
            self.app.ui(ui);
        });
        let (typing_area, copied) = ui.ctx().output(|o| {
            let copied = o.commands.iter().rev().find_map(|c| match c {
                egui::OutputCommand::CopyText(t) if !t.is_empty() && t != PASTE_SEED => Some(t.clone()),
                _ => None,
            });
            (o.ime.as_ref().map(|ime| ime.rect), copied)
        });
        // The shell decides when the keyboard shows, not egui-winit (which would show it whenever
        // a field has focus, also when the app gave it focus without a tap).
        ui.ctx().output_mut(|o| o.ime = None);
        if let Some(text) = copied
            && let Err(e) = bridge::set_clipboard("", &text)
        {
            log::warn!("clipboard: {e}");
        }
        if !self.seeded {
            self.seeded = true;
            ui.ctx().copy_text(PASTE_SEED.to_string());
        }
        self.keyboard_follows(&ui.ctx().clone(), typing_area, released, now);
        self.after_app();
    }

    fn on_exit(&mut self) {
        if self.waiting.get() > 0 {
            log::info!("closing with {} picker or save answers outstanding", self.waiting.get());
        }
        self.store_prefs();
    }
}

/// The settings as they would be written, to notice a change.
fn settings_json(app: &SlideApp) -> Option<String> {
    let ui = serde_json::to_string(&app.ui).ok()?;
    let prefs = serde_json::to_string(&app.session.prefs).ok()?;
    Some(format!("{ui}\n{prefs}"))
}

/// What to tell the user about a presentation whose own file lacks its saved changes.
fn not_saved_message(path: &str, state: State) -> String {
    let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if state == State::Conflict {
        format!("Not saved to “{name}”: another app changed that file. Your changes are kept here; save them with File › Save As.")
    } else {
        format!("Not saved to “{name}” yet: writing to that file failed. Save again, or keep a copy with File › Save As.")
    }
}

/// A small text file (the clipboard mirror), if it exists and is under 16 MB.
fn read_small(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > 16 << 20 {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// The clipboard mirror's text (the activity refreshes it whenever the app comes to the front or
/// the clipboard changes; Android lets only the focused app read it). `None` when empty.
fn read_text(path: &Path) -> Option<String> {
    read_small(path).filter(|t| !t.is_empty())
}

/// Wake the app whenever the Java side leaves a message: its answers arrive while the app may be
/// idle, waiting for input. A glance at the folder every few milliseconds costs next to nothing.
fn watch_inbox(dir: PathBuf, ctx: egui::Context) {
    let spawned = std::thread::Builder::new().name("inbox".into()).spawn(move || {
        loop {
            if mailbox::has_mail(&dir) {
                ctx.request_repaint();
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    });
    if let Err(e) = spawned {
        log::warn!("inbox watcher: {e}");
    }
}

fn clear_dir(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        if let Err(err) = std::fs::remove_file(e.path()) {
            log::warn!("cannot remove {}: {err}", e.path().display());
        }
    }
}

fn services(dirs: &Dirs, inbox: Inbox, waiting: Rc<Cell<u32>>, messages: Rc<RefCell<Vec<String>>>, saves: Rc<RefCell<Saves>>) -> Services {
    let open_waiting = waiting.clone();
    let (open_messages, save_messages) = (messages.clone(), messages);
    let open_inbox = inbox.clone();
    let outbox = dirs.outbox.clone();
    let print_outbox = dirs.outbox.clone();
    let clipboard_html = dirs.clipboard_html.clone();
    Services {
        open_async: Some(Box::new(move |purpose: &str, id: u64| match bridge::open(purpose, id) {
            Ok(()) => open_waiting.set(open_waiting.get().saturating_add(1)),
            Err(e) => {
                log::error!("cannot show the file picker: {e}");
                open_inbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(InboxEvent::Cancelled { id });
                open_messages.borrow_mut().push(format!("Couldn't show the file picker: {e}"));
            }
        })),
        download: Some(Box::new(move |name: &str, bytes: &[u8]| {
            if let Err(e) = save(&outbox, &saves, &waiting, name, bytes) {
                log::error!("save {name}: {e}");
                save_messages.borrow_mut().push(format!("Couldn't save {name}: {e}"));
            }
        })),
        print: Some(Box::new(move |bytes: &[u8]| {
            let file = print_outbox.join("print.pdf");
            std::fs::write(&file, bytes).map_err(|e| e.to_string())?;
            bridge::print(&file.to_string_lossy(), "DeckCraft presentation")
        })),
        clipboard_html: Some(Box::new(move || read_text(&clipboard_html))),
        clipboard_set_html: Some(Box::new(|html: &str, text: &str| bridge::set_clipboard(html, text).is_ok())),
        inbox: Some(inbox),
        ..Default::default()
    }
}

/// Hand `bytes` to the activity's save picker as `name`. The answer comes later, through the
/// inbox, under the save's ticket.
fn save(outbox: &Path, saves: &Rc<RefCell<Saves>>, waiting: &Rc<Cell<u32>>, name: &str, bytes: &[u8]) -> Result<(), String> {
    let name = mailbox::safe_name(name);
    let ticket = {
        let mut s = saves.borrow_mut();
        s.next = s.next.saturating_add(1);
        s.next
    };
    let file = outbox.join(format!("{ticket}-{name}"));
    std::fs::write(&file, bytes).map_err(|e| format!("cannot stage the file: {e}"))?;
    if let Err(e) = bridge::save(ticket, &file.to_string_lossy(), &name, mailbox::mime_for(&name)) {
        let _ = std::fs::remove_file(&file);
        return Err(e);
    }
    saves.borrow_mut().jobs.insert(ticket, SaveJob { name, doc: None });
    waiting.set(waiting.get().saturating_add(1));
    Ok(())
}
