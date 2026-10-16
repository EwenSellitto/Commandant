//! `commandant gui`: the desktop window.
//!
//! GPUI owns the main thread; `main` runs it outside the tokio runtime, which
//! is left for the core's calls to the server. What those report is awaited
//! in a GPUI task, which hands it to the core and redraws.

mod asks;
mod chat;
mod connected;
mod login;
mod placement;
mod rows;
mod sidebar;
mod thread;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use commandant_client_core::config::{self, Client};
use commandant_common::dirs;
use gpui::{
    App, AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowOptions, div,
    point, px, size,
};
use gpui_component::{ActiveTheme as _, Root, Theme, ThemeMode};
use tokio::runtime::Handle;

use self::connected::Connected;
use self::login::Card;
use self::placement::{MIN_HEIGHT, MIN_WIDTH, Placement};
use crate::cli::{ClientArgs, GuiArgs, UiArgs};

/// The window's size on a first run, before it has a saved placement.
const FIRST_SIZE: (f32, f32) = (1200.0, 800.0);

/// How long the window stays put before its placement is saved.
const SAVE_AFTER: Duration = Duration::from_millis(500);

/// Opens the window and returns once it is closed; the core's calls are
/// made on `handle`. Linux keeps it in the foreground, so `--foreground`
/// changes nothing yet.
pub fn run(args: GuiArgs, client: &ClientArgs, handle: Handle) -> Result<()> {
    // With no display server GPUI runs headless: an invisible window that
    // never closes.
    #[cfg(target_os = "linux")]
    if gpui::guess_compositor() == "Headless" {
        anyhow::bail!(
            "no display to open a window on (neither WAYLAND_DISPLAY nor DISPLAY is set)"
        );
    }
    let placement_file = dirs::gui_window()?;
    // Flags and the environment name the server as for any command; a
    // broken config file is said on the card, which then asks for a link.
    let saved = config::find(
        client.addr.clone(),
        client.token.clone(),
        client.token_file.clone(),
    );
    let failed = Rc::new(RefCell::new(None));
    let failure = failed.clone();
    let app = gpui_platform::application().with_assets(gpui_kit_assets::Assets);
    app.run(move |cx| {
        gpui_component::init(cx);
        asks::bind_keys(cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let options = WindowOptions {
            window_bounds: Some(window_bounds(&placement_file, cx)),
            window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
            titlebar: Some(TitlebarOptions {
                title: Some("Commandant".into()),
                ..Default::default()
            }),
            app_id: Some("commandant".into()),
            ..Default::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            // Dark whatever the system says: many Linux desktops don't tell
            // GPUI, which then takes them for light.
            Theme::change(ThemeMode::Dark, Some(window), cx);
            let shell = cx.new(|cx| Shell::new(placement_file, args.ui, handle, saved, window, cx));
            cx.new(|cx| Root::new(shell, window, cx))
        });
        if let Err(e) = opened {
            *failure.borrow_mut() = Some(e);
            cx.quit();
        }
    });
    match failed.take() {
        Some(e) => Err(anyhow!("cannot open the window: {e:#}")),
        None => Ok(()),
    }
}

/// Where the window opens: where it last was, or centred when that is
/// nowhere on today's displays.
fn window_bounds(placement_file: &Path, cx: &App) -> WindowBounds {
    let centred = || WindowBounds::centered(size(px(FIRST_SIZE.0), px(FIRST_SIZE.1)), cx);
    let Some(saved) = Placement::load(placement_file) else {
        return centred();
    };
    let bounds = Bounds::new(
        point(px(saved.x), px(saved.y)),
        size(px(saved.width), px(saved.height)),
    );
    let displays = cx.displays();
    if !displays.is_empty() && !displays.iter().any(|d| d.bounds().intersects(&bounds)) {
        return centred();
    }
    if saved.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    }
}

fn placement_of(window: &Window) -> Placement {
    let state = window.window_bounds();
    let bounds = state.get_bounds();
    Placement {
        x: bounds.origin.x.into(),
        y: bounds.origin.y.into(),
        width: bounds.size.width.into(),
        height: bounds.size.height.into(),
        maximized: matches!(state, WindowBounds::Maximized(_)),
    }
}

/// A window that can't be placed next time just opens centred, so failing to
/// save is only logged.
fn save(placement: Placement, placement_file: &Path) {
    if let Err(e) = placement.save(placement_file) {
        eprintln!("warning: cannot save the window's placement: {e:#}");
    }
}

/// The window's content: the login card, then the core's views.
struct Shell {
    screen: Screen,
    /// Where the core makes its calls.
    handle: Handle,
    /// Where the views start.
    args: UiArgs,
    placement_file: PathBuf,
    /// Saves the placement once the window has stopped moving, so it is kept
    /// even when the process is stopped rather than the window closed. It
    /// runs on the main thread, as the save on closing does, so the two
    /// never write at once.
    saving: Option<Task<()>>,
    _placing: Subscription,
}

impl Shell {
    fn new(
        placement_file: PathBuf,
        args: UiArgs,
        handle: Handle,
        saved: Result<Option<Client>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let on_close = placement_file.clone();
        window.on_window_should_close(cx, move |window, _| {
            save(placement_of(window), &on_close);
            true
        });
        let placing = cx.observe_window_bounds(window, |shell, window, cx| {
            shell.save_soon(window, cx);
        });
        let (saved, error) = match saved {
            Ok(saved) => (saved, None),
            Err(e) => (None, Some(format!("{e:#}").into())),
        };
        let try_saved = saved.is_some();
        let mut shell = Self {
            screen: Screen::LogIn(Card::new(saved, error, window, cx)),
            handle,
            args,
            placement_file,
            saving: None,
            _placing: placing,
        };
        if try_saved {
            shell.retry(window, cx);
        }
        shell
    }

    fn save_soon(&mut self, window: &Window, cx: &mut Context<Self>) {
        let placement = placement_of(window);
        let file = self.placement_file.clone();
        let settled = cx.background_executor().timer(SAVE_AFTER);
        // Replacing the task drops, and so cancels, the previous one.
        self.saving = Some(cx.spawn(async move |_, _| {
            settled.await;
            save(placement, &file);
        }));
    }
}

/// What the window shows.
enum Screen {
    LogIn(Card),
    Connected(Entity<Connected>),
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let base = div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground);
        match &self.screen {
            Screen::LogIn(card) => base.child(self.render_card(card, cx)),
            Screen::Connected(connected) => base.child(connected.clone()),
        }
    }
}
