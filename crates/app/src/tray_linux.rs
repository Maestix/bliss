//! The tray on Linux: an AppIndicator with a menu of the desktop's own.
//!
//! Windows' tray (`Tray` in ui.rs, rule 23 of AGENTS.md §6) is the window
//! itself in a small shape, put beside the icon when the icon is clicked. None
//! of that can be done here:
//!
//! - **The icon reports no clicks.** On Linux tray-icon drives an
//!   AppIndicator — a StatusNotifierItem on the session bus — and the host
//!   that draws it opens the item's menu itself. No event reaches the program,
//!   and there is no position to put a window at; Wayland would not let a
//!   window place itself there anyway.
//! - **No menu, no icon.** libayatana-appindicator registers an item only once
//!   it has a menu, so an icon built without one, as Windows' is, never shows.
//! - **No library, no program.** tray-icon loads the library when the icon is
//!   built and panics if it is not there, so `host()` looks for it first.
//!
//! So the icon carries a native menu of the program's main switches, kept in
//! step with the engine's frames by `Model`, which the frame loop in `App`
//! updates whether or not the window can be seen — the menu is wanted most
//! when it cannot. Its items change in place (muda keeps their GTK widgets),
//! and its clicks arrive through `use_muda_event_handler`.

use std::cell::RefCell;
use std::ffi::CString;
use std::rc::Rc;
use std::time::{Duration, Instant};

use bliss_proto::ProxyMode;
use dioxus::desktop::muda::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
};
use dioxus::desktop::trayicon::{TrayIcon, TrayIconBuilder};
use dioxus::prelude::*;

use super::{Command, Controller, Frame, catch_up, quit, tray_tooltip};

/// The most servers the menu lists. A subscription brings hundreds, and a
/// native menu taller than the screen scrolls by arrows a line at a time;
/// the rest are a click away, in the window.
const SERVERS: usize = 30;

/// What the menu shows, from the frame. Compared before anything is changed,
/// so a frame that moves only the rates leaves the menu alone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    status: String,
    on: bool,
    /// Name, whether in use, and the place in the frame's list a click
    /// selects: the starred servers come first, so the two orders differ.
    servers: Vec<(String, bool, usize)>,
    /// Servers past `SERVERS`.
    more: usize,
    presets: Vec<(String, bool)>,
    drifted: bool,
    proxy: ProxyMode,
}

/// The menu's model, kept by the frame loop.
pub static MODEL: GlobalSignal<Model> = Signal::global(Model::default);

pub fn model(frame: &Frame) -> Model {
    let current = frame.routing.current_preset.as_deref();
    Model {
        status: tray_tooltip(frame),
        on: frame.state.is_on(),
        servers: super::favorites_first(&frame.servers)
            .into_iter()
            .take(SERVERS)
            .map(|(index, server)| (server.tag.clone(), server.current, index))
            .collect(),
        more: frame.servers.len().saturating_sub(SERVERS),
        presets: frame
            .routing
            .presets
            .iter()
            .map(|name| (name.clone(), current == Some(name.as_str())))
            .collect(),
        drifted: frame.routing.drifted,
        proxy: frame.proxy_mode,
    }
}

/// How long a start at sign-in waits for the desktop's tray. Plasma starts
/// the programs of a session beside the service that takes their icons, and
/// a bliss asked to start in the tray that came first would otherwise open
/// minimized, with a close button that quits, for the whole session.
const SIGN_IN: Duration = Duration::from_secs(10);

/// Whether an icon can be shown at all: the library tray-icon loads is there,
/// and something on the session bus draws StatusNotifierItems. GNOME as it
/// ships has no such host; Ubuntu's and KDE's do. `at_sign_in` waits for the
/// host a while (`SIGN_IN`), if the library is there.
pub fn host(at_sign_in: bool) -> bool {
    library() && watcher(if at_sign_in { SIGN_IN } else { Duration::ZERO })
}

/// tray-icon tries these, in this order, and panics after the last.
fn library() -> bool {
    [
        "libayatana-appindicator3.so.1",
        "libappindicator3.so.1",
        "libayatana-appindicator3.so",
        "libappindicator3.so",
    ]
    .into_iter()
    .any(|name| {
        let Ok(name) = CString::new(name) else {
            return false;
        };
        // SAFETY: a NUL-terminated name; the handle, when there is one, is
        // closed at once.
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
        if handle.is_null() {
            return false;
        }
        unsafe { libc::dlclose(handle) };
        true
    })
}

/// Is anyone on the session bus taking StatusNotifierItems? Asked again
/// for up to `wait`, until someone is.
fn watcher(wait: Duration) -> bool {
    use gtk::gio;
    use gtk::glib;
    use gtk::glib::ToVariant;

    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) else {
        return false;
    };
    let until = Instant::now() + wait;
    loop {
        let asked = bus.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&("org.kde.StatusNotifierWatcher",).to_variant()),
            glib::VariantTy::new("(b)").ok(),
            gio::DBusCallFlags::NONE,
            1000,
            None::<&gio::Cancellable>,
        );
        if asked
            .ok()
            .and_then(|reply| reply.get::<(bool,)>())
            .is_some_and(|(owned,)| owned)
        {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Bring the window up and to the front, with the activation token a second
/// copy passed on, if any.
///
/// Through GTK rather than tao: `present` shows, de-iconifies and raises in
/// one call, where tao's `set_visible` and `set_focus` are requests queued for
/// later, and a focus asked for before the window is visible is dropped.
///
/// What is inside is shown first, as tao's own showing does: a window that
/// started in the tray was never shown, and GTK's `present` shows the window
/// alone — the box tao packs the page into stayed hidden, and the window
/// came up empty.
pub fn present(window: &dioxus::desktop::DesktopContext, token: Option<&str>) {
    use dioxus::desktop::tao::platform::unix::WindowExtUnix;
    use gtk::prelude::*;

    let shown = window.window.gtk_window();
    if let Some(token) = token {
        shown.set_startup_id(token);
    }
    shown.show_all();
    shown.deiconify();
    shown.present();
}

/// The icon and every item that changes, held for as long as the program runs.
struct Items {
    _icon: TrayIcon,
    status: MenuItem,
    toggle: MenuItem,
    servers: Submenu,
    server_items: Vec<CheckMenuItem>,
    listed: Vec<String>,
    more: Option<MenuItem>,
    more_count: usize,
    presets: Submenu,
    preset_items: Vec<CheckMenuItem>,
    preset_names: Vec<String>,
    proxy: Vec<(ProxyMode, CheckMenuItem)>,
}

const OPEN: &str = "bliss:open";
const TOGGLE: &str = "bliss:toggle";
const QUIT: &str = "bliss:quit";
const SERVER: &str = "bliss:server:";
const PRESET: &str = "bliss:preset:";
const PROXY: &str = "bliss:proxy:";

const MODES: [ProxyMode; 3] = [ProxyMode::Unchanged, ProxyMode::Set, ProxyMode::Clear];

impl Items {
    fn build() -> Option<Self> {
        let menu = Menu::new();
        let status = MenuItem::new("bliss", false, None);
        let toggle = MenuItem::with_id(TOGGLE, "Подключить", true, None);
        let open = MenuItem::with_id(OPEN, "Открыть bliss", true, None);
        let servers = Submenu::new("Серверы", true);
        let presets = Submenu::new("Пресеты правил", false);
        let proxies = Submenu::new("Системный прокси", true);
        let proxy: Vec<(ProxyMode, CheckMenuItem)> = MODES
            .into_iter()
            .map(|mode| {
                let item = CheckMenuItem::with_id(
                    format!("{PROXY}{}", mode.key()),
                    mode.label(),
                    true,
                    false,
                    None,
                );
                (mode, item)
            })
            .collect();
        for (_, item) in &proxy {
            proxies.append(item).ok()?;
        }
        let quit = MenuItem::with_id(QUIT, "Выход", true, None);
        let items: [&dyn IsMenuItem; 9] = [
            &status,
            &toggle,
            &open,
            &PredefinedMenuItem::separator(),
            &servers,
            &presets,
            &proxies,
            &PredefinedMenuItem::separator(),
            &quit,
        ];
        for item in items {
            menu.append(item).ok()?;
        }

        let mut builder = TrayIconBuilder::new()
            .with_id("bliss")
            .with_menu(Box::new(menu))
            // A folder of the executable's own for the icon's picture, which
            // the icon is shown from, so a probe copy cannot delete the one
            // the user's bliss shows.
            .with_temp_dir_path(picture_folder());
        if let Some(mark) = mark() {
            builder = builder.with_icon(mark);
        }
        let icon = builder
            .build()
            .inspect_err(|e| tracing::warn!("no tray icon: {e}"))
            .ok()?;
        Some(Self {
            _icon: icon,
            status,
            toggle,
            servers,
            server_items: Vec::new(),
            listed: Vec::new(),
            more: None,
            more_count: 0,
            presets,
            preset_items: Vec::new(),
            preset_names: Vec::new(),
            proxy,
        })
    }

    /// Bring every item in step with `model`. The lists are made again only
    /// when their names change; the ticks are set every time, since a check
    /// item ticks or unticks itself when it is clicked, and what is chosen is
    /// the engine's to say.
    fn show(&mut self, model: &Model) {
        self.status.set_text(&model.status);
        self.toggle.set_text(if model.on {
            "Отключить"
        } else {
            "Подключить"
        });

        let names: Vec<String> = model
            .servers
            .iter()
            .map(|(tag, _, index)| format!("{index}:{tag}"))
            .collect();
        if names != self.listed || model.more != self.more_count {
            for item in self.server_items.drain(..) {
                let _ = self.servers.remove(&item);
            }
            if let Some(more) = self.more.take() {
                let _ = self.servers.remove(&more);
            }
            for (tag, _, index) in &model.servers {
                let item = CheckMenuItem::with_id(
                    format!("{SERVER}{index}"),
                    label(tag),
                    true,
                    false,
                    None,
                );
                let _ = self.servers.append(&item);
                self.server_items.push(item);
            }
            if model.more > 0 {
                let more = MenuItem::new(format!("…и ещё {} — в окне", model.more), false, None);
                let _ = self.servers.append(&more);
                self.more = Some(more);
            }
            self.servers.set_enabled(!model.servers.is_empty());
            self.listed = names;
            self.more_count = model.more;
        }
        for (item, (_, current, _)) in self.server_items.iter().zip(&model.servers) {
            item.set_checked(*current);
        }

        let names: Vec<String> = model.presets.iter().map(|(name, _)| name.clone()).collect();
        if names != self.preset_names {
            for item in self.preset_items.drain(..) {
                let _ = self.presets.remove(&item);
            }
            for (index, (name, _)) in model.presets.iter().enumerate() {
                let item = CheckMenuItem::with_id(
                    format!("{PRESET}{index}"),
                    label(name),
                    true,
                    false,
                    None,
                );
                let _ = self.presets.append(&item);
                self.preset_items.push(item);
            }
            self.presets.set_enabled(!model.presets.is_empty());
            self.preset_names = names;
        }
        for (item, (name, current)) in self.preset_items.iter().zip(&model.presets) {
            item.set_checked(*current);
            // The window's violet dot, in words: the switches have moved since.
            item.set_text(if *current && model.drifted {
                format!("{} (изменён)", label(name))
            } else {
                label(name)
            });
        }

        for (mode, item) in &self.proxy {
            item.set_checked(*mode == model.proxy);
        }
    }
}

/// A name as a menu shows it. muda turns `&` into GTK's mnemonic and doubles
/// every `_` itself, so an `&` is doubled here — which muda turns back into
/// one — and an `_` is left alone: doubled twice, every underscore in a
/// server's name showed as two. A name a subscription gave can also be as
/// long as it likes.
fn label(name: &str) -> String {
    // Cut first: cut after, a doubled `&` could lose its second half.
    let shown = if name.chars().count() > 48 {
        let kept: String = name.chars().take(47).collect();
        format!("{kept}…")
    } else {
        name.to_owned()
    };
    shown.replace('&', "&&")
}

/// The mark, as the window's own icon.
fn mark() -> Option<dioxus::desktop::trayicon::Icon> {
    dioxus::desktop::icon_from_memory(super::MARK)
        .inspect_err(|e| tracing::warn!("could not load the tray icon: {e}"))
        .ok()
}

fn picture_folder() -> std::path::PathBuf {
    let stem = std::env::current_exe()
        .ok()
        .and_then(|path| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "bliss".to_owned());
    directories::BaseDirs::new()
        .and_then(|base| {
            base.runtime_dir()
                .map(|dir| dir.to_path_buf())
                .or_else(|| Some(base.cache_dir().to_path_buf()))
        })
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("{stem}-tray"))
}

/// The icon beside the clock and its menu, and a second copy's asks to show
/// the window.
#[component]
pub fn Tray(frame: Signal<Frame>) -> Element {
    let controller = use_context::<Controller>();
    let window = dioxus::desktop::use_window();
    let items: Rc<RefCell<Option<Items>>> = use_hook(|| {
        Rc::new(RefCell::new(
            if super::TRAY_HOST.load(std::sync::atomic::Ordering::Relaxed) {
                Items::build()
            } else {
                None
            },
        ))
    });

    // Before the frame loop's first model, the frame the page started with.
    let model = match MODEL() {
        model if model == Model::default() => self::model(&frame.peek()),
        model => model,
    };
    if let Some(items) = items.borrow_mut().as_mut() {
        items.show(&model);
    }

    let clicked = items.clone();
    let opening = window.clone();
    let asking = controller.clone();
    dioxus::desktop::use_muda_event_handler(move |event: &MenuEvent| {
        let id = event.id.0.as_str();
        let model = MODEL.peek().clone();
        match id {
            OPEN => {
                catch_up(&opening, frame, &asking);
                present(&opening, None);
            }
            TOGGLE => asking.send(Command::Toggle { was_on: model.on }),
            QUIT => quit(&opening),
            _ => {
                if let Some(index) = id.strip_prefix(SERVER).and_then(|i| i.parse().ok()) {
                    asking.send(Command::Select(index));
                } else if let Some((name, _)) = id
                    .strip_prefix(PRESET)
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| model.presets.get(i))
                {
                    asking.send(Command::ApplyPreset(name.clone()));
                } else if let Some(mode) = id
                    .strip_prefix(PROXY)
                    .and_then(|key| MODES.into_iter().find(|mode| mode.key() == key))
                {
                    asking.send(Command::Proxy(mode));
                } else {
                    return;
                }
            }
        }
        if let Some(items) = clicked.borrow_mut().as_mut() {
            items.show(&model);
        }
    });

    // A second copy asked for the window (`instance.rs`).
    let raising = window.clone();
    use_future(move || {
        let window = raising.clone();
        let controller = controller.clone();
        async move {
            let Some(mut raises) = super::RAISES.lock().ok().and_then(|mut r| r.take()) else {
                return;
            };
            while let Some(token) = raises.recv().await {
                tracing::info!("window: another copy asked for it");
                catch_up(&window, frame, &controller);
                present(&window, token.as_deref());
            }
        }
    });

    rsx! {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name the menu would underline a letter of, or run off the screen
    /// with, is shown as written and cut.
    #[test]
    fn a_name_is_shown_as_written() {
        assert_eq!(label("RU_Moscow_1"), "RU_Moscow_1", "muda escapes it");
        assert_eq!(label("US & EU"), "US && EU", "not a mnemonic");
        let long = "я".repeat(60);
        let shown = label(&long);
        assert_eq!(shown.chars().count(), 48);
        assert!(shown.ends_with('…'));
    }
}
