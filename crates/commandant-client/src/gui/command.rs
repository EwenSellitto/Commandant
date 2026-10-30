//! A command's block: the command as its header, what it printed with its
//! error output tinted, a Stop button while it runs, and how it ended. A
//! chat's thread shows one per `!command`; a task's output view fills its
//! pane with one.

use commandant_client_core::chat::{Ended, Run};
use gpui::{App, ClickEvent, Div, FontWeight, Hsla, ParentElement as _, Styled as _, Window, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _, h_flex, v_flex};

/// The block of `run`, which ran `line`; `stop` is what its Stop button
/// does.
pub(super) fn render_command(
    line: &str,
    run: &Run,
    stop: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    render_printed(run, Some(command_header(line, run, stop, cx)), cx)
}

/// The command and, while it runs, its Stop button: atop its block, or
/// kept in view over an output view's.
pub(super) fn command_header(
    line: &str,
    run: &Run,
    stop: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    let stopping = run.stopping();
    let stop = run.running().then(|| {
        Button::new(("stop-run", u64::from(run.id)))
            .danger()
            .xsmall()
            .icon(IconName::Square)
            .label(if stopping { "Stopping…" } else { "Stop" })
            .disabled(stopping)
            .on_click(stop)
    });
    h_flex()
        .gap_2()
        .justify_between()
        .font_family(theme.mono_font_family.clone())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .font_weight(FontWeight::SEMIBOLD)
                .child(format!("$ {line}")),
        )
        .children(stop)
}

/// What `run` printed, with its error output tinted, and how it ended,
/// laid under `header`.
pub(super) fn render_printed(run: &Run, header: Option<Div>, cx: &App) -> Div {
    let theme = cx.theme();
    let output = run.output.iter().map(|printed| {
        let color = match printed.stderr {
            true => theme.danger,
            false => theme.foreground,
        };
        div()
            .text_color(color)
            .child(printed.text.trim_end_matches('\n').to_string())
    });
    let dropped = run.dropped.then(|| {
        div()
            .italic()
            .text_color(theme.muted_foreground)
            .child("… its earlier output wasn't kept")
    });
    let (end, color) = end_label(run, cx);
    v_flex()
        .p_2()
        .gap_1()
        .rounded_md()
        .bg(theme.muted)
        .font_family(theme.mono_font_family.clone())
        .text_sm()
        .children(header)
        .children(dropped)
        .children(output)
        .child(div().text_xs().text_color(color).child(end))
}

/// How a command ended, or that it still runs, and its color.
pub(super) fn end_label(run: &Run, cx: &App) -> (String, Hsla) {
    let theme = cx.theme();
    match &run.ended {
        None if run.stopping() => ("stopping…".into(), theme.warning),
        None => ("running…".into(), theme.info),
        Some(ended) if ended.succeeded() => ("exit 0".into(), theme.muted_foreground),
        Some(Ended::Exited(code)) => (format!("exit {code}"), theme.danger),
        Some(Ended::Cancelled) => ("cancelled".into(), theme.warning),
        Some(Ended::Failed(why)) => (why.clone(), theme.danger),
    }
}
