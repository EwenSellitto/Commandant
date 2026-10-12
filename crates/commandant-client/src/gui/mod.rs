//! `commandant gui`: the desktop window.
//!
//! GPUI owns the main thread; `main` runs it outside the tokio runtime, which
//! is left for the calls to the server once the GUI makes any.

mod placement;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use commandant_common::dirs;
use gpui::{
    App, AppContext as _, Bounds, Context, IntoElement, Render, Styled as _, Subscription, Task,
    TitlebarOptions, Window, WindowBounds, WindowOptions, div, point, px, size,
};
use gpui_component::{ActiveTheme as _, Root, Theme};

use self::placement::{MIN_HEIGHT, MIN_WIDTH, Placement};
use crate::cli::GuiArgs;

/// The window's size on a first run, before it has a saved placement.
const FIRST_SIZE: (f32, f32) = (1200.0, 800.0);

/// How long the window stays put before its placement is saved.
const SAVE_AFTER: Duration = Duration::from_millis(500);

/// Opens the window and returns once it is closed. Linux keeps it in the
/// foreground, so `--foreground` changes nothing yet; the other arguments
/// wait for the views.
pub fn run(_args: GuiArgs) -> Result<()> {
    // With no display server GPUI runs headless: an invisible window that
    // never closes.
    #[cfg(target_os = "linux")]
    if gpui::guess_compositor() == "Headless" {
        anyhow::bail!(
            "no display to open a window on (neither WAYLAND_DISPLAY nor DISPLAY is set)"
        );
    }
    let placement_file = dirs::gui_window()?;
    let failed = Rc::new(RefCell::new(None));
    let failure = failed.clone();
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
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
            Theme::sync_system_appearance(Some(window), cx);
            let shell = cx.new(|cx| Shell::new(placement_file, window, cx));
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

/// The window's content: empty until the views come.
struct Shell {
    placement_file: PathBuf,
    /// Saves the placement once the window has stopped moving, so it is kept
    /// even when the process is stopped rather than the window closed. It
    /// runs on the main thread, as the save on closing does, so the two
    /// never write at once.
    saving: Option<Task<()>>,
    _subscriptions: [Subscription; 2],
}

impl Shell {
    fn new(placement_file: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let on_close = placement_file.clone();
        window.on_window_should_close(cx, move |window, _| {
            save(placement_of(window), &on_close);
            true
        });
        let subscriptions = [
            cx.observe_window_appearance(window, |_, window, cx| {
                Theme::sync_system_appearance(Some(window), cx);
            }),
            cx.observe_window_bounds(window, |shell, window, cx| {
                shell.save_soon(window, cx);
            }),
        ];
        Self {
            placement_file,
            saving: None,
            _subscriptions: subscriptions,
        }
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

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
    }
}
