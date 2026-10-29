//! Logging in: the card a link is pasted on at first run, and that offers
//! to retry when the saved server can't be reached.

use commandant_client_core::config::{self, Client};
use commandant_client_core::{Core, Updates};
use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, SharedString, Styled as _,
    Subscription, Task, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme as _, Disableable as _, h_flex, v_flex};

use super::{Connected, LoggedOut, Resized, Screen, Shell};

/// The card's width, in logical pixels.
const WIDTH: f32 = 480.0;

pub(super) struct Card {
    /// The pasted link, or the saved server's.
    link: Entity<InputState>,
    /// The saved server, while it is the one tried; none on a first run,
    /// or once a different link is wanted.
    saved: Option<Client>,
    error: Option<SharedString>,
    /// The connection being tried; dropping it gives up on it.
    connecting: Option<Task<()>>,
    _enter: Subscription,
}

impl Card {
    /// The card for `saved`, or for a first run when there is none.
    pub(super) fn new(
        saved: Option<Client>,
        error: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) -> Self {
        let link = cx.new(|cx| {
            let state = InputState::new(window, cx).placeholder("commandant://…");
            match &saved {
                Some(client) => state.default_value(client.link()),
                None => state,
            }
        });
        let enter = cx.subscribe_in(&link, window, |shell, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                shell.connect_pasted(window, cx);
            }
        });
        if saved.is_none() {
            link.update(cx, |link, cx| link.focus(window, cx));
        }
        Self {
            link,
            saved,
            error,
            connecting: None,
            _enter: enter,
        }
    }
}

impl Shell {
    fn card(&mut self) -> Option<&mut Card> {
        match &mut self.screen {
            Screen::LogIn(card) => Some(card),
            Screen::Connected(_) => None,
        }
    }

    /// Connects with the link in the field, and saves it once it works.
    fn connect_pasted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(card) = self.card() else { return };
        if card.saved.is_some() || card.connecting.is_some() {
            return;
        }
        let text = card.link.read(cx).value();
        match Client::from_link(&text) {
            Ok(client) => {
                let connecting = self.connect(client, true, window, cx);
                self.trying(connecting);
            }
            Err(e) => {
                card.error = Some(format!("{e:#}").into());
            }
        }
        cx.notify();
    }

    /// Tries the saved server, again after a failure.
    pub(super) fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(saved) = self.card().and_then(|card| card.saved.clone()) else {
            return;
        };
        let connecting = self.connect(saved, false, window, cx);
        self.trying(connecting);
        cx.notify();
    }

    /// Gives up on the saved server, for a link to paste.
    fn use_another_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(card) = self.card() else { return };
        card.saved = None;
        card.error = None;
        card.connecting = None;
        card.link.update(cx, |link, cx| {
            link.set_value("", window, cx);
            link.focus(window, cx);
        });
        cx.notify();
    }

    fn trying(&mut self, connecting: Task<()>) {
        if let Some(card) = self.card() {
            card.error = None;
            card.connecting = Some(connecting);
        }
    }

    /// Starts a core on `client`, in the background; once it is up the
    /// shell shows it, and `remember` saves the server for the next run.
    fn connect(
        &self,
        client: Client,
        remember: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let defaults = self.args.defaults();
        let handle = self.handle.clone();
        cx.spawn_in(window, async move |shell, cx| {
            let started = Core::start(&client, defaults, handle).await;
            let _ = shell.update_in(cx, |shell, window, cx| {
                match started {
                    Ok((core, updates)) => {
                        if remember && let Err(e) = config::remember(&client) {
                            eprintln!("warning: cannot save the server's link: {e:#}");
                        }
                        shell.connected(client, core, updates, window, cx);
                    }
                    Err(e) => {
                        if let Some(card) = shell.card() {
                            card.connecting = None;
                            card.error = Some(format!("{e:#}").into());
                        }
                    }
                }
                cx.notify();
            });
        })
    }

    /// Shows the core on `client`, opening the chat the command line names,
    /// if any.
    fn connected(
        &mut self,
        client: Client,
        core: Core,
        updates: Updates,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.args.first_open(core.nodes()).unwrap_or_else(|e| {
            eprintln!("warning: {e:#}");
            None
        });
        let sidebar = self.sidebar;
        let connected = cx.new(|cx| Connected::new(core, updates, open, sidebar, window, cx));
        let resizing = cx.subscribe_in(
            &connected,
            window,
            |shell, _, Resized(width), window, cx| {
                shell.sidebar = *width;
                shell.save_soon(window, cx);
            },
        );
        let logging_out = cx.subscribe_in(
            &connected,
            window,
            move |shell, _, LoggedOut, window, cx| shell.log_out(&client, window, cx),
        );
        self._watching = vec![resizing, logging_out];
        self.screen = Screen::Connected(connected);
    }

    /// Forgets the server the core, now shut down, was on, and asks for a
    /// link.
    fn log_out(&mut self, client: &Client, window: &mut Window, cx: &mut Context<Self>) {
        // A server flags or the environment named keeps the saved one.
        let error = config::forget_if_saved(client)
            .err()
            .map(|e| format!("couldn't forget the server: {e:#}").into());
        self._watching.clear();
        self.screen = Screen::LogIn(Card::new(None, error, window, cx));
        cx.notify();
    }

    pub(super) fn render_card(&self, card: &Card, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let connecting = card.connecting.is_some();
        let (label, hint) = match &card.saved {
            Some(_) => ("Retry", "The saved server's link."),
            None => (
                "Connect",
                "Paste the connection link `commandant-server serve` printed.",
            ),
        };
        let label = if connecting { "Connecting…" } else { label };
        let primary = Button::new("connect")
            .primary()
            .label(label)
            .loading(connecting)
            .disabled(connecting);
        let primary = match card.saved {
            Some(_) => {
                primary.on_click(cx.listener(|shell, _, window, cx| shell.retry(window, cx)))
            }
            None => primary
                .on_click(cx.listener(|shell, _, window, cx| shell.connect_pasted(window, cx))),
        };
        let buttons = h_flex().gap_2().justify_end().child(primary);
        let buttons = match card.saved {
            Some(_) => buttons.child(
                Button::new("another-link")
                    .ghost()
                    .label("Use a different link")
                    .on_click(
                        cx.listener(|shell, _, window, cx| shell.use_another_link(window, cx)),
                    ),
            ),
            None => buttons,
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .w(px(WIDTH))
                    .gap_3()
                    .p_6()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(div().text_lg().child("Connect to your server"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(hint),
                    )
                    .child(Input::new(&card.link).disabled(card.saved.is_some() || connecting))
                    .children(
                        card.error
                            .clone()
                            .map(|error| div().text_sm().text_color(theme.danger).child(error)),
                    )
                    .child(buttons),
            )
    }
}
