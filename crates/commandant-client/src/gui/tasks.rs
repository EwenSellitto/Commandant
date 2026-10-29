//! The tasks: everything the nodes have run, newest first, refreshed while
//! shown; and one command's output, followed while it runs.

use std::time::Duration;

use commandant_client_core::chat::elapsed;
use commandant_client_core::{Intent, OutputId, what_ran};
use commandant_common::time::{self, ago};
use commandant_proto::TaskInfo;
use gpui::{
    App, Context, Div, FontWeight, Hsla, InteractiveElement as _, ParentElement as _, SharedString,
    Stateful, StatefulInteractiveElement as _, Styled as _, WeakEntity, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::clipboard::Clipboard;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};

use super::command::{command_header, end_label, render_printed};
use super::connected::Connected;

/// The widths of the columns before what ran, in logical pixels.
const NODE: f32 = 140.0;
const KIND: f32 = 70.0;
const STATUS: f32 = 90.0;
const AGE: f32 = 70.0;
/// Room for a running task's ×, so rows line up.
const CANCEL: f32 = 24.0;

impl Connected {
    /// The tasks as last listed, with the node they are filtered by.
    pub(super) fn render_tasks(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let Some(tasks) = self.core.tasks() else {
            return div();
        };
        let nodes = self.core.nodes();
        let whose = match &tasks.node {
            None => "Every node".to_string(),
            Some(id) => nodes
                .iter()
                .find(|n| &n.id == id)
                .map_or_else(|| id.clone(), |n| n.name.clone()),
        };
        // Every node first, then each node: what the menu offers.
        let choices: Vec<(String, Option<String>)> = std::iter::once(("Every node".into(), None))
            .chain(nodes.iter().map(|n| (n.name.clone(), Some(n.id.clone()))))
            .collect();
        let current = tasks.node.clone();
        let connected = cx.entity().downgrade();
        let filter = Button::new("tasks-of")
            .ghost()
            .small()
            .label(whose)
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for (label, node) in &choices {
                    let (connected, node) = (connected.clone(), node.clone());
                    let item = PopupMenuItem::new(label.clone())
                        .checked(node == current)
                        .on_click(move |_, window, cx| {
                            let node = node.clone();
                            let _ = connected.update(cx, |this, cx| {
                                this.act(Intent::ListTasks { node }, window, cx)
                            });
                        });
                    menu = menu.item(item);
                }
                menu
            });
        let header = h_flex()
            .px_4()
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Tasks"))
            .child(filter)
            .children(
                tasks
                    .loading
                    .then(|| Spinner::new().xsmall().color(theme.info)),
            );
        let heading = |text: &'static str, width: f32| div().w(px(width)).flex_none().child(text);
        let headings = h_flex()
            .gap_2()
            .px_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(heading("Node", NODE))
            .child(heading("Kind", KIND))
            .child(heading("Status", STATUS))
            .child(heading("Started", AGE))
            .child(div().flex_1().child("What ran"))
            .child(div().w(px(CANCEL)).flex_none());
        let empty = (tasks.tasks.is_empty() && !tasks.loading).then(|| {
            div()
                .px_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Nothing has run yet. Run a command with `!` in a chat.")
        });
        // The core's notices are toasts.
        let said = tasks.error.as_ref().map(|e| {
            let said = format!("couldn't list the tasks: {e}");
            div()
                .px_4()
                .py_1()
                .text_sm()
                .text_color(theme.warning)
                .child(said)
        });
        let rows: Vec<_> = tasks
            .tasks
            .iter()
            .map(|task| self.task_row(task, cx))
            .collect();
        v_flex()
            .size_full()
            .child(header)
            .child(
                v_flex()
                    .id("tasks")
                    .flex_1()
                    .min_h_0()
                    .p_2()
                    .gap_0p5()
                    .overflow_y_scroll()
                    .child(headings)
                    .children(rows)
                    .children(empty),
            )
            .children(said)
    }

    /// A task's node, kind, how it ended, when it started and what ran; a
    /// click opens it, and × cancels it while it runs. Greyed once its
    /// output is pruned. Known by its id, as rows move when the list refreshes.
    fn task_row(&self, task: &TaskInfo, cx: &Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let id = task.id.clone();
        let open = cx.listener(move |this, _, window, cx| {
            this.act(Intent::OpenTask(id.clone()), window, cx)
        });
        let id = task.id.clone();
        let cancel = (task.status == "running").then(|| {
            Button::new(SharedString::from(format!("cancel-{}", task.id)))
                .ghost()
                .xsmall()
                .icon(IconName::Close)
                .tooltip("Cancel")
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.act(Intent::CancelTask(id.clone()), window, cx);
                }))
        });
        let (status, color) = status(task, cx);
        let column = |width: f32| div().w(px(width)).flex_none().truncate();
        h_flex()
            .id(SharedString::from(format!("task-{}", task.id)))
            .gap_2()
            .px_2()
            .h(px(28.0))
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .hover(|s| s.bg(theme.accent))
            .on_click(open)
            .when(task.output_pruned, |row| row.opacity(0.5))
            .child(
                column(NODE)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(task.node_name.clone()),
            )
            .child(
                column(KIND)
                    .text_color(theme.muted_foreground)
                    .child(commandant_common::or(&task.kind, "command").to_string()),
            )
            .child(column(STATUS).text_color(color).child(status))
            .child(
                column(AGE)
                    .text_color(theme.muted_foreground)
                    .child(ago(task.created_at)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(what_ran(task).replace(['\r', '\n'], " ")),
            )
            .child(div().w(px(CANCEL)).flex_none().children(cancel))
    }

    /// A command's output filling the pane: what ran, where, for how long
    /// and how it ended over what it printed, kept at the end unless
    /// scrolled up.
    pub(super) fn render_output(&self, id: OutputId, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let Some(output) = self.core.output(id) else {
            return div();
        };
        let back = match self.nodes_shown && self.core.tasks().is_none() {
            true => "Back to the nodes",
            false => "Back to the tasks",
        };
        let back = Button::new("back")
            .ghost()
            .small()
            .icon(IconName::ArrowLeft)
            .tooltip(back)
            .on_click(cx.listener(|this, _, window, cx| this.close_output(window, cx)));
        let cwd = commandant_common::or(&output.cwd, "worker's directory");
        let took = output.finished.unwrap_or_else(time::now) - output.started;
        let took = elapsed(Duration::from_secs(took.max(0) as u64));
        let facts = [output.node.as_str(), cwd, &took].join(" · ");
        let (ended, color) = end_label(&output.run, cx);
        let connected = cx.entity().downgrade();
        let task = output.run.task().cloned();
        let stop = cx.listener(move |this, _, window, cx| {
            if let Some(task) = task.clone() {
                this.act(Intent::CancelTask(task), window, cx);
            }
        });
        let header = v_flex()
            .px_4()
            .py_2()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex().gap_2().child(back).child(
                    command_header(&output.line, &output.run, stop, cx)
                        .flex_1()
                        .min_w_0(),
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(div().flex_1().min_w_0().truncate().child(facts))
                    .child(div().text_color(color).child(ended))
                    .child(
                        Clipboard::new("copy-output")
                            .tooltip("Copy all")
                            .value_fn(move |_, cx| printed(&connected, id, cx)),
                    ),
            );
        let pruned = output.pruned.then(|| {
            div()
                .italic()
                .text_color(theme.muted_foreground)
                .child("output pruned: the server no longer keeps it")
        });
        // At the end when last laid out: kept there as more comes.
        let scroll = &self.output_scroll;
        if -scroll.offset().y >= scroll.max_offset().y - px(1.0) {
            scroll.scroll_to_bottom();
        }
        v_flex().size_full().child(header).child(
            v_flex()
                .id("output")
                .flex_1()
                .min_h_0()
                .p_4()
                .gap_2()
                .overflow_y_scroll()
                .track_scroll(scroll)
                .children(pruned)
                .child(render_printed(&output.run, None, cx)),
        )
    }
}

/// All output `id` printed, read only when copied: it can be a megabyte.
fn printed(connected: &WeakEntity<Connected>, id: OutputId, cx: &App) -> SharedString {
    let output = connected.upgrade();
    let output = output.as_ref().and_then(|c| c.read(cx).core.output(id));
    output.map_or_else(SharedString::default, |o| {
        o.run
            .output
            .iter()
            .map(|p| p.text.as_str())
            .collect::<String>()
            .into()
    })
}

/// `running`, `exit 0`, `exit 3`, `cancelled`, and its color.
fn status(task: &TaskInfo, cx: &App) -> (String, Hsla) {
    let theme = cx.theme();
    match (task.status.as_str(), task.exit_code) {
        ("running", _) => ("running".into(), theme.info),
        ("succeeded", _) => ("exit 0".into(), theme.muted_foreground),
        ("failed", Some(code)) => (format!("exit {code}"), theme.danger),
        ("cancelled", _) => ("cancelled".into(), theme.warning),
        (status, _) => (status.into(), theme.danger),
    }
}
