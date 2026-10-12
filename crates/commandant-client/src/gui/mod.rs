//! `commandant gui`: the desktop window.
//!
//! GPUI owns the main thread; the tokio runtime `main` built keeps running
//! beside it for the calls to the server.

mod placement;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
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
/// foreground, so `--foreground` changes nothing yet.
pub fn run(_args: GuiArgs) -> Result<()> {
    let placement_file = dirs::gui_window()?;
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
            let on_close = placement_file.clone();
            window.on_window_should_close(cx, move |window, _| {
                save(placement_of(window), &on_close);
                true
            });
            let shell = cx.new(|cx| Shell::new(placement_file, window, cx));
            cx.new(|cx| Root::new(shell, window, cx))
        });
        if let Err(e) = opened {
            eprintln!("error: cannot open the window: {e:#}");
            cx.quit();
        }
    });
    Ok(())
}

/// Where the window opens: where it last closed, or centred.
fn window_bounds(placement_file: &Path, cx: &App) -> WindowBounds {
    let Some(saved) = Placement::load(placement_file) else {
        return WindowBounds::centered(size(px(FIRST_SIZE.0), px(FIRST_SIZE.1)), cx);
    };
    let bounds = Bounds::new(
        point(px(saved.x), px(saved.y)),
        size(px(saved.width), px(saved.height)),
    );
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
    /// even when the process is stopped rather than the window closed.
    saving: Option<Task<()>>,
    _subscriptions: [Subscription; 2],
}

impl Shell {
    fn new(placement_file: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
        self.saving = Some(cx.background_spawn(async move {
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
