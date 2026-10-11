use super::*;
use crate::state::fixtures::{acme, method, output, started};
use crate::state::{Ask, Choose, Edit, Wanted};

fn chat() -> Chat {
    let node = NodeInfo {
        id: "n1".into(),
        name: "w1".into(),
        harnesses: vec!["opencode".into()],
        ..Default::default()
    };
    Chat::new(1, node, Settings::default(), None)
}

/// What submitting `text` comes to.
fn command(chat: &mut Chat, text: &str) -> Outcome {
    let (outcome, app) = chat.submit(text);
    assert_eq!(app, None, "{text:?} is the chat's");
    outcome
}

/// The one effect an outcome has, if any.
fn effect(outcome: Outcome) -> Option<Effect> {
    let mut effects = outcome.effects.into_iter();
    let effect = effects.next();
    assert!(effects.next().is_none());
    effect
}

/// Sends `text` and returns the request.
fn send(chat: &mut Chat, text: &str) -> PromptRequest {
    let outcome = command(chat, text);
    assert_eq!(outcome.prompt, Some((chat.id, Edit::Clear)));
    match effect(outcome) {
        Some(Effect::Send(_, request)) => request,
        _ => panic!("{text:?} should be sent"),
    }
}

fn choose(chat: &mut Chat, choice: Choose) -> Option<Effect> {
    effect(chat.choose(choice))
}

#[test]
fn a_turn_streams_into_the_thread_and_keeps_the_session() {
    let mut app = chat();
    let request = send(&mut app, "hi");
    assert_eq!(
        (request.prompt.as_str(), request.node.as_str()),
        ("hi", "n1")
    );

    // Sending while the agent works leaves the text in the prompt.
    let kept = command(&mut app, "next");
    assert!(kept.effects.is_empty() && kept.prompt.is_none());

    app.on_message(started("t1"));
    // "é" split across two chunks.
    app.on_message(output(OutputStream::Reasoning, b"let me think"));
    app.on_message(output(OutputStream::Stdout, b"Hello caf\xc3"));
    app.on_message(output(OutputStream::Stderr, b"[opencode] write a.txt\n"));
    app.on_message(output(OutputStream::Stdout, b"\xa9 done"));
    app.on_message(Message::Task(TaskEvent::Finished(TaskFinished {
        task_id: "t1".into(),
        exit_code: Some(0),
        session_id: "ses_1".into(),
        model: "a/smart".into(),
        usage: Some(AgentUsage {
            input: 1200,
            output: 34,
            cost: 0.5,
            context: 1234,
            ..Default::default()
        }),
        ..Default::default()
    })));

    let summary = app.thread.last().unwrap().text.clone();
    assert!(
        summary.starts_with("a/smart · ") && summary.ends_with(" · 1.2k in · 34 out · $0.50"),
        "{summary}"
    );
    let thread: Vec<_> = app
        .thread
        .iter()
        .map(|e| (e.role, e.text.as_str()))
        .collect();
    assert_eq!(
        thread,
        [
            (Role::User, "hi"),
            (Role::Thinking, "let me think"),
            (Role::Agent, "Hello caf"),
            (Role::Tool, "write a.txt"),
            (Role::Agent, "é done"),
            (Role::Summary, summary.as_str()),
        ]
    );
    assert_eq!((app.spent, app.context), (0.5, 1234));
    assert!(matches!(app.activity, Activity::Idle));
    assert_eq!(app.unseen, Some(Unseen::Done));

    // The next prompt continues the session.
    assert_eq!(send(&mut app, "next").session_id, "ses_1");
    assert_eq!(app.sent(), ["hi", "next"], "sent once each");
}

#[test]
fn cancelling_cancels_the_task_once() {
    let mut app = chat();
    send(&mut app, "hi");
    assert!(app.on_message(started("t1")).is_none());
    assert_eq!(app.cancel(), [Effect::Cancel(1, "t1".into())]);
    assert!(app.cancel().is_empty());
}

#[test]
fn cancelling_before_the_task_starts_cancels_it_when_it_does() {
    let mut app = chat();
    send(&mut app, "hi");
    assert!(app.cancel().is_empty());
    assert!(matches!(
        app.activity,
        Activity::Working {
            cancelling: true,
            ..
        }
    ));
    assert!(matches!(
        app.on_message(started("t1")),
        Some(Effect::Cancel(_, id)) if id == "t1"
    ));
}

fn with_options() -> Chat {
    let mut app = chat();
    let model = |id: &str, variants: &[&str]| ModelChoice {
        id: id.into(),
        name: id.into(),
        variants: variants.iter().map(|v| v.to_string()).collect(),
        ..Default::default()
    };
    let agent = |name: &str| AgentChoice {
        name: name.into(),
        ..Default::default()
    };
    app.on_message(Message::Options(Ok(Arc::new(AgentOptions {
        agents: vec![agent("build"), agent("plan"), agent("review")],
        models: vec![model("a/fast", &[]), model("a/smart", &["low", "high"])],
        default_agent: "build".into(),
        commands: vec![AgentCommand {
            name: "review".into(),
            description: "Review the changes".into(),
            source: "skill".into(),
        }],
        mcp_servers: vec![McpServer {
            name: "docs".into(),
            status: "disabled".into(),
            ..Default::default()
        }],
        ..Default::default()
    }))));
    app
}

#[test]
fn the_agents_commands_run_with_their_arguments() {
    let mut app = with_options();
    let request = send(&mut app, "/review  the parser ");
    assert_eq!(
        (request.command.as_str(), request.prompt.as_str()),
        ("review", "the parser")
    );
    assert_eq!(app.thread.last().unwrap().text, "/review  the parser");
    app.on_message(Message::Failed("stop".into()));

    // Any other slash is just text.
    let request = send(&mut app, "/etc/hosts?");
    assert_eq!(
        (request.command.as_str(), request.prompt.as_str()),
        ("", "/etc/hosts?")
    );
    app.on_message(Message::Failed("stop".into()));

    // Picking one readies it for its arguments.
    command(&mut app, "/skills");
    let pick = app.ask().and_then(Ask::choices).unwrap();
    assert_eq!(pick.choices.len(), 1);
    let picked = app.choose(pick.choices[0].value.clone());
    assert_eq!(
        picked.prompt,
        Some((app.id, Edit::Insert("/review ".into())))
    );
    assert!(app.ask().is_none());
}

#[test]
fn mcp_servers_are_switched_from_a_picker() {
    let mut app = with_options();
    command(&mut app, "/mcp");
    assert_eq!(app.ask().and_then(Ask::choices).unwrap().choices.len(), 1);
    let Some(Effect::SwitchMcp {
        chat,
        name,
        connect,
        ..
    }) = choose(&mut app, Choose::Mcp("docs".into()))
    else {
        panic!("choosing a server switches it");
    };
    assert_eq!((chat, name.as_str(), connect), (app.id, "docs", true));

    let mut options = (**app.options.as_ref().unwrap()).clone();
    options.mcp_servers[0].status = "connected".into();
    let switched = Ok(Arc::new(options));
    app.on_message(Message::Options(switched.clone()));
    app.mcp_switched("docs", &switched);
    assert_eq!(app.thread.last().unwrap().text, "docs: connected");
    app.mcp_switched("docs", &Err("timed out".into()));
    assert_eq!(
        app.thread.last().unwrap().text,
        "couldn't switch docs: timed out"
    );

    assert!(matches!(
        choose(&mut app, Choose::Mcp("docs".into())),
        Some(Effect::SwitchMcp { connect: false, .. })
    ));
}

#[test]
fn agents_cycle_once_the_node_has_said_which() {
    // A new chat is still asking.
    let mut app = chat();
    assert!(app.cycle_agent(1).is_none());
    assert_eq!(app.thread.last().unwrap().role, Role::Info);

    let mut app = with_options();
    assert_eq!(app.agent(), "build");
    app.cycle_agent(1);
    assert_eq!(app.agent(), "plan");
    app.cycle_agent(-1);
    app.cycle_agent(-1);
    assert_eq!(app.agent(), "review");

    // A failed fetch can be retried.
    let mut failed = chat();
    failed.on_message(Message::Options(Err("offline".into())));
    assert!(matches!(
        failed.cycle_agent(1),
        Some(Effect::FetchOptions { .. })
    ));
}

#[test]
fn model_and_effort_are_picked() {
    let mut app = with_options();
    // The default model is unknown until a reply names it.
    command(&mut app, "/effort");
    assert!(app.ask().is_none());

    let outcome = command(&mut app, "/model smart");
    assert_eq!(outcome.prompt, Some((app.id, Edit::Clear)));
    let pick = app
        .ask()
        .and_then(Ask::choices)
        .expect("/model opens a picker");
    assert_eq!((pick.title, pick.filter.as_str()), ("Model", "smart"));
    assert_eq!(pick.current, Some(Choose::Model(String::new())));
    choose(&mut app, Choose::Model("a/smart".into()));
    assert!(app.ask().is_none());
    assert_eq!(app.settings.model, "a/smart");

    command(&mut app, "/effort");
    let efforts: Vec<_> = app
        .ask()
        .and_then(Ask::choices)
        .unwrap()
        .choices
        .iter()
        .map(|c| c.label.as_str())
        .collect();
    assert_eq!(efforts, ["Default", "low", "high"]);
    choose(&mut app, Choose::Effort("high".into()));
    assert_eq!(app.settings.effort, "high");
    // Cycling wraps round to the model's default.
    app.cycle_effort();
    assert_eq!(app.settings.effort, "");
    app.cycle_effort();
    assert_eq!(app.settings.effort, "low");

    // A model without that effort drops it.
    choose(&mut app, Choose::Model("a/fast".into()));
    assert_eq!(app.settings.model, "a/fast");
    assert_eq!(app.settings.effort, "");

    let request = send(&mut app, "hi");
    assert_eq!(request.model, "a/fast");
    assert_eq!(request.agent, "");
}

#[test]
fn odd_input_is_handled() {
    let mut app = chat();
    // Blank input, and a lone slash, send nothing useful.
    let blank = command(&mut app, "   ");
    assert!(blank.effects.is_empty() && blank.prompt.is_none());
    assert!(app.sent().is_empty());
    let request = send(&mut app, "/");
    assert_eq!(
        (request.prompt.as_str(), request.command.as_str()),
        ("/", "")
    );
    app.on_message(Message::Failed("stop".into()));

    // Before the node has listed its commands, a command is just text.
    let request = send(&mut app, "/review x");
    assert_eq!(
        (request.prompt.as_str(), request.command.as_str()),
        ("/review x", "")
    );
}

#[test]
fn the_apps_commands_go_to_the_app() {
    let mut app = chat();
    for (text, expected) in [
        ("/quit", AppCommand::Quit),
        ("/exit", AppCommand::Quit),
        ("/new", AppCommand::New),
        ("/sessions", AppCommand::Sessions),
        ("/nodes", AppCommand::Nodes),
        ("/close", AppCommand::Close),
        ("/help", AppCommand::Help),
        ("/?", AppCommand::Help),
    ] {
        let (outcome, command) = app.submit(text);
        assert_eq!(command, Some(expected), "{text}");
        // Quitting leaves the prompt as it is.
        let cleared = expected != AppCommand::Quit;
        assert_eq!(outcome.prompt.is_some(), cleared, "{text}");
    }
}

#[test]
fn the_title_is_the_first_prompt_sent() {
    let mut app = with_options();
    command(&mut app, "/model");
    app.dismiss();
    assert!(app.ask().is_none());
    assert_eq!(app.title, "", "commands aren't prompts");
    send(&mut app, "first");
    // Sent while it works: kept, not sent, not the title.
    assert!(command(&mut app, "second").effects.is_empty());
    app.on_message(Message::Failed("stop".into()));
    send(&mut app, "third");
    assert_eq!(app.title, "first");
}

#[test]
fn a_failed_stream_ends_the_turn_and_marks_it() {
    let mut app = chat();
    send(&mut app, "hi");
    app.on_message(started("t1"));
    app.on_message(output(OutputStream::Stdout, b"partial \xc3"));
    app.on_message(Message::Failed("the stream ended".into()));
    assert!(matches!(app.activity, Activity::Idle));
    assert_eq!(app.unseen, Some(Unseen::Failed));
    assert!(app.running_task().is_none());
    // Cancelling after the turn ended does nothing.
    assert!(app.cancel().is_empty());
    // The half character is dropped, not glued to the next reply.
    send(&mut app, "again");
    app.on_message(output(OutputStream::Stdout, b"ok"));
    assert_eq!(app.thread.last().unwrap().text, "ok");
}

#[test]
fn a_picker_asked_for_too_early_opens_when_the_options_come() {
    let mut app = chat();
    // The node hasn't said yet: the request is remembered, not dropped.
    assert!(effect(command(&mut app, "/model smart")).is_none());
    assert!(app.ask().is_none());
    assert!(
        effect(command(&mut app, "/model smart")).is_none(),
        "still the one request"
    );
    let options = AgentOptions {
        models: vec![ModelChoice {
            id: "a/smart".into(),
            name: "Smart".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    app.on_message(Message::Options(Ok(Arc::new(options))));
    let pick = app
        .ask()
        .and_then(Ask::choices)
        .expect("it opens by itself");
    assert_eq!((pick.title, pick.filter.as_str()), ("Model", "smart"));

    // If the node can't say, the wait ends with why.
    let mut failed = chat();
    command(&mut failed, "/agent");
    failed.on_message(Message::Options(Err("timed out".into())));
    assert!(failed.ask().is_none());
    let said = &failed.thread.last().unwrap().text;
    assert!(
        said.contains("couldn't list the agent's options: timed out"),
        "{said}"
    );
}

fn listed(app: &mut Chat) {
    assert!(matches!(
        effect(command(app, "/providers")),
        Some(Effect::FetchProviders { .. })
    ));
    let methods = vec![
        method("Browser", true, 0),
        method("Code", true, 1),
        method("API key", false, 2),
    ];
    let providers = vec![ModelProvider {
        connected: true,
        ..acme(methods)
    }];
    assert!(app.on_message(Message::Providers(Ok(providers))).is_none());
    assert!(app.ask().is_some(), "the providers come in a picker");
    choose(app, Choose::Provider("acme".into()));
    // Browser, Code, API key, Sign out.
    assert_eq!(app.ask().and_then(Ask::choices).unwrap().choices.len(), 4);
}

fn sent(effect: Option<Effect>) -> (String, auth_action::Action) {
    match effect {
        Some(Effect::Authenticate {
            provider, action, ..
        }) => (provider, action.action.unwrap()),
        _ => panic!("a sign-in step is sent"),
    }
}

fn sign_in(provider: &str, oauth: Option<u32>) -> Choose {
    Choose::SignIn {
        provider: provider.into(),
        oauth,
    }
}

#[test]
fn an_api_key_is_sent_but_never_shown() {
    let mut app = with_options();
    listed(&mut app);
    assert!(choose(&mut app, sign_in("acme", None)).is_none());
    let Some(Ask::Enter {
        label,
        secret: true,
        wanted: Wanted::ApiKey { provider },
    }) = app.ask()
    else {
        panic!("a key is asked for, secretly");
    };
    assert_eq!(
        (label.as_str(), provider.as_str()),
        ("the API key for Acme", "acme")
    );
    assert!(app.completions("/").is_empty(), "a key isn't a command");
    // Submitted as a prompt by mistake, it is still the key.
    let mut submitted = with_options();
    listed(&mut submitted);
    choose(&mut submitted, sign_in("acme", None));
    let typed = command(&mut submitted, "sk-secret");
    assert_eq!(
        typed.prompt,
        Some((submitted.id, Edit::Clear)),
        "not left in view"
    );
    let (_, action) = sent(effect(typed));
    assert_eq!(action, auth_action::Action::ApiKey("sk-secret".into()));
    assert!(!submitted.sent().iter().any(|s| s.contains("sk-secret")));
    // A blank line isn't one.
    assert!(app.enter("  ").effects.is_empty());
    let (provider, action) = sent(effect(app.enter("sk-secret")));
    assert_eq!(provider, "acme");
    assert_eq!(action, auth_action::Action::ApiKey("sk-secret".into()));
    assert!(app.thread.iter().all(|e| !e.text.contains("sk-secret")));
    assert!(app.sent().iter().all(|s| !s.contains("sk-secret")));

    // Done: the node's models are asked for again.
    let done = app.on_message(Message::Auth(Ok(ProviderAuthResult::default())));
    assert!(matches!(done, Some(Effect::FetchOptions { .. })));
    assert!(app.ask().is_none() && app.signing_in().is_none());

    // Dismissing backs out of typing a key.
    listed(&mut app);
    choose(&mut app, sign_in("acme", None));
    app.dismiss();
    assert!(app.ask().is_none());
    assert_eq!(app.thread.last().unwrap().text, "not signed in");
    // With nothing asked, a line is a prompt again.
    assert!(app.enter("sk-secret").effects.is_empty());
}

#[test]
fn oauth_waits_in_the_browser_or_takes_the_code() {
    let mut app = with_options();
    listed(&mut app);
    let (_, start) = sent(choose(&mut app, sign_in("acme", Some(0))));
    assert_eq!(start, auth_action::Action::OauthStart(0));
    let started = ProviderAuthResult {
        url: "https://acme.test/authorize?redirect_uri=http://localhost:1455".into(),
        ..Default::default()
    };
    // It finishes by itself, once the user is done in the browser.
    let (_, finish) = sent(app.on_message(Message::Auth(Ok(started))));
    assert!(matches!(finish, auth_action::Action::OauthFinish(f) if f.code.is_empty()));
    assert!(app.thread.iter().any(|e| e.text.contains("acme.test")));
    assert!(app.thread.iter().any(|e| e.text.contains("headless")));
    app.on_message(Message::Auth(Err("timed out".into())));
    assert!(
        app.thread
            .last()
            .unwrap()
            .text
            .contains("couldn't sign in to Acme: timed out")
    );

    listed(&mut app);
    sent(choose(&mut app, sign_in("acme", Some(1))));
    let started = ProviderAuthResult {
        url: "https://acme.test/code".into(),
        needs_code: true,
        ..Default::default()
    };
    assert!(app.on_message(Message::Auth(Ok(started))).is_none());
    assert!(matches!(app.ask(), Some(Ask::Enter { secret: false, .. })));
    let (_, finish) = sent(effect(app.enter("abc")));
    assert_eq!(app.signing_in(), Some("signing in to Acme…"));
    assert!(
        matches!(finish, auth_action::Action::OauthFinish(f) if f.index == 1 && f.code == "abc")
    );
}

#[test]
fn slash_commands_complete_by_name() {
    let app = with_options();
    let names =
        |text: &str| -> Vec<String> { app.completions(text).into_iter().map(|(n, _)| n).collect() };
    assert_eq!(names("/re"), ["review"], "the agent's own commands too");
    assert_eq!(names("/s"), ["skills", "sessions"]);
    assert!(
        names("/review ").is_empty(),
        "nothing to complete past the name"
    );
    assert!(names("/zzz").is_empty());
    assert!(names("re").is_empty());

    assert_eq!(app.command_len("/model smart"), 6);
    assert_eq!(app.command_len("/review it"), 7);
    assert_eq!(app.command_len("/nope x"), 0);
}

#[test]
fn projects_are_browsed_joined_or_copied() {
    let mut app = with_options();
    assert!(matches!(
        effect(command(&mut app, "/project")),
        Some(Effect::FetchProjects { .. })
    ));
    assert!(
        effect(command(&mut app, "/project")).is_none(),
        "one listing at a time"
    );
    let copy = |id: &str, sessions: &[&str]| ProjectCopy {
        id: id.into(),
        path: format!("/state/projects/app/{id}"),
        branch: "main".into(),
        sessions: sessions.iter().map(|s| s.to_string()).collect(),
    };
    let projects = vec![commandant_proto::Project {
        name: "app".into(),
        repository: "https://example.com/me/app.git".into(),
        copies: vec![
            copy("3fa9c1d2", &["fix the parser", "add tests", "docs"]),
            copy("77aa00bb", &[]),
        ],
    }];
    app.on_message(Message::Projects(Ok(projects)));
    let pick = app
        .ask()
        .and_then(Ask::choices)
        .expect("the projects to browse");
    let shown: Vec<_> = pick
        .choices
        .iter()
        .map(|c| (c.label.clone(), c.detail.clone()))
        .collect();
    assert_eq!(shown[0].0, "+ new copy of app");
    assert_eq!(
        shown[1],
        (
            "app · 3fa9c1d2".into(),
            "main · fix the parser; add tests +1".into()
        )
    );
    assert_eq!(shown[2].1, "main · no sessions yet");

    // Joining a copy works in it at once.
    let join = pick.choices[1].value.clone();
    assert!(choose(&mut app, join).is_none());
    assert_eq!(app.settings.cwd, "/state/projects/app/3fa9c1d2");

    // A new copy is cloned first, by name or by URL.
    let Some(Effect::PrepareProject { repository, .. }) = effect(command(&mut app, "/project app"))
    else {
        panic!("a new copy is asked for");
    };
    assert_eq!(repository, "app");
    assert!(app.preparing.is_some());
    let ready = ProjectReady {
        id: "c0ffee00".into(),
        path: "/state/projects/app/c0ffee00".into(),
        ..Default::default()
    };
    app.on_message(Message::Project(Ok(ready)));
    assert!(app.preparing.is_none());
    assert_eq!(send(&mut app, "hi").cwd, "/state/projects/app/c0ffee00");

    // Once the session has started, a new one is needed to move.
    app.on_message(Message::Task(TaskEvent::Finished(TaskFinished {
        exit_code: Some(0),
        session_id: "ses_1".into(),
        ..Default::default()
    })));
    assert!(effect(command(&mut app, "/project")).is_none());
    assert!(app.thread.last().unwrap().text.contains("Ctrl-N"));
    let mut empty = with_options();
    command(&mut empty, "/project");
    empty.on_message(Message::Projects(Ok(Vec::new())));
    assert!(empty.ask().is_none());
    assert!(
        empty
            .thread
            .last()
            .unwrap()
            .text
            .contains("no projects yet")
    );
}

#[test]
fn the_agents_commands_are_known_once_loaded() {
    assert_eq!(chat().agent_commands().count(), 0);
    let app = with_options();
    let commands: Vec<_> = app.agent_commands().collect();
    assert_eq!(commands, [("review", "Review the changes")]);
    assert!(app.is_command("review") && app.is_command("login"));
}

/// Runs `line` with `!`, and returns the run and its request.
fn run(chat: &mut Chat, line: &str) -> (u64, RunCommandRequest) {
    let outcome = command(chat, &format!("!{line}"));
    assert_eq!(outcome.prompt, Some((chat.id, Edit::Clear)));
    match effect(outcome) {
        Some(Effect::RunCommand { run, request, .. }) => (run, request),
        _ => panic!("{line:?} should run"),
    }
}

/// What a command's task reports, as `message` says it for the agent's.
fn to_run(run: u64, message: Message) -> Message {
    match message {
        Message::Task(event) => Message::Run(run, Ok(event)),
        _ => unreachable!("a task event"),
    }
}

fn finished(exit_code: Option<i32>, error: &str, cancelled: bool) -> Message {
    Message::Task(TaskEvent::Finished(TaskFinished {
        exit_code,
        error: error.into(),
        cancelled,
        ..Default::default()
    }))
}

/// The block of the command that ran `line`.
fn block<'a>(chat: &'a Chat, line: &str) -> &'a Run {
    let entry = chat.thread.iter().find(|e| e.text == line);
    let entry = entry.unwrap_or_else(|| panic!("no block for {line:?}"));
    assert_eq!(entry.role, Role::Command);
    entry.run.as_ref().unwrap()
}

fn printed(run: &Run) -> Vec<(bool, &str)> {
    run.output
        .iter()
        .map(|p| (p.stderr, p.text.as_str()))
        .collect()
}

#[test]
fn a_command_runs_on_the_node_in_the_chats_directory() {
    let mut app = chat();
    app.settings.cwd = "/src/app".into();
    let (_, request) = run(&mut app, " ls -la ");
    assert_eq!(request.node, "n1");
    assert_eq!(request.argv, ["sh", "-c", "ls -la"]);
    assert_eq!(request.cwd, "/src/app");
    assert!(request.env.is_empty());

    // A block of its own; nothing goes to the agent, which isn't at work,
    // and the title waits for a prompt.
    assert!(block(&app, "ls -la").running());
    assert!(!app.activity.working());
    assert_eq!(app.title, "");
    assert_eq!(app.sent(), ["! ls -la"]);
}

#[test]
fn a_bang_alone_says_what_it_is_for() {
    let mut app = chat();
    for text in ["!", "!   "] {
        let outcome = command(&mut app, text);
        assert!(outcome.effects.is_empty() && outcome.prompt.is_none());
        let said = app.thread.last().unwrap();
        assert_eq!(said.role, Role::Info);
        assert!(said.text.contains("shell command"), "{}", said.text);
    }
}

#[test]
fn a_commands_output_streams_into_its_block_until_it_exits() {
    let mut app = chat();
    let (id, _) = run(&mut app, "make");
    app.on_message(to_run(id, started("t2")));
    // "é" split across two chunks, with error output between them.
    app.on_message(to_run(
        id,
        output(OutputStream::Stdout, b"building caf\xc3"),
    ));
    app.on_message(to_run(id, output(OutputStream::Stderr, b"warning\n")));
    app.on_message(to_run(id, output(OutputStream::Stdout, b"\xa9\n")));
    app.on_message(to_run(id, output(OutputStream::Stdout, b"done\n")));
    assert!(app.unseen.is_none());
    app.on_message(to_run(id, finished(Some(2), "", false)));

    let make = block(&app, "make");
    assert_eq!(
        printed(make),
        [
            (false, "building caf"),
            (true, "warning\n"),
            (false, "é\ndone\n")
        ]
    );
    assert_eq!(make.ended, Some(Ended::Exited(2)));
    assert_eq!(app.unseen, Some(Unseen::Failed));

    let (id, _) = run(&mut app, "true");
    app.on_message(to_run(id, finished(Some(0), "", false)));
    assert_eq!(block(&app, "true").ended, Some(Ended::Exited(0)));
    assert_eq!(app.unseen, Some(Unseen::Done));
}

#[test]
fn a_block_keeps_the_end_of_a_long_output() {
    let mut app = chat();
    let (id, _) = run(&mut app, "yes");
    let line = format!("{}\n", "y".repeat(99));
    for _ in 0..1000 {
        app.on_message(to_run(id, output(OutputStream::Stdout, line.as_bytes())));
    }
    app.on_message(to_run(id, output(OutputStream::Stderr, b"oops\n")));
    let yes = block(&app, "yes");
    let kept: usize = yes.output.iter().map(|p| p.text.len()).sum();
    assert!(kept <= 64 * 1024 && kept > 60 * 1024, "{kept}");
    assert!(yes.dropped);
    // From the start of a line, to the very end.
    assert!(yes.output[0].text.starts_with(&line));
    assert_eq!(yes.output.last().unwrap().text, "oops\n");
}

#[test]
fn a_command_that_cannot_finish_says_why() {
    let mut app = chat();
    let (id, _) = run(&mut app, "yes");
    app.on_message(Message::Run(id, Err("the stream broke".into())));
    let failed = Ended::Failed("the stream broke".into());
    assert_eq!(block(&app, "yes").ended, Some(failed));

    let (id, _) = run(&mut app, "kill -9 $$");
    let killed = finished(None, "terminated by signal 9", false);
    app.on_message(to_run(id, killed));
    let failed = Ended::Failed("terminated by signal 9".into());
    assert_eq!(block(&app, "kill -9 $$").ended, Some(failed.clone()));
    // What comes after the end changes nothing.
    app.on_message(to_run(id, output(OutputStream::Stdout, b"late")));
    app.on_message(to_run(id, finished(Some(0), "", false)));
    let killed = block(&app, "kill -9 $$");
    assert_eq!(
        (killed.ended.clone(), killed.output.len()),
        (Some(failed), 0)
    );
}

#[test]
fn esc_stops_the_commands_and_then_the_turn() {
    let mut app = chat();
    send(&mut app, "hi");
    app.on_message(started("t1"));
    // A command runs while the agent works; one not started yet is
    // stopped once it is.
    let (one, _) = run(&mut app, "sleep 9");
    app.on_message(to_run(one, started("t2")));
    let (two, _) = run(&mut app, "sleep 8");
    assert_eq!(app.cancel(), [Effect::Cancel(1, "t2".into())]);
    assert_eq!(
        app.on_message(to_run(two, started("t3"))),
        Some(Effect::Cancel(1, "t3".into()))
    );
    assert!(app.cancel().is_empty(), "they are stopping");
    assert!(app.running_task().is_some(), "the turn goes on");

    app.on_message(to_run(one, finished(None, "", true)));
    app.on_message(to_run(two, finished(None, "", true)));
    assert_eq!(block(&app, "sleep 9").ended, Some(Ended::Cancelled));
    assert_eq!(app.cancel(), [Effect::Cancel(1, "t1".into())]);
}

#[test]
fn a_command_that_will_not_stop_can_be_stopped_again() {
    let mut app = chat();
    let (id, _) = run(&mut app, "sleep 9");
    app.on_message(to_run(id, started("t2")));
    assert_eq!(app.cancel(), [Effect::Cancel(1, "t2".into())]);
    let refused = Message::CancelFailed {
        task_id: "t2".into(),
        error: "no route".into(),
    };
    app.on_message(refused);
    let said = app.thread.last().unwrap();
    assert_eq!(
        (said.role, said.text.as_str()),
        (Role::Error, "couldn't stop sleep 9: no route")
    );
    assert!(block(&app, "sleep 9").running());
    assert_eq!(app.cancel(), [Effect::Cancel(1, "t2".into())]);

    // The turn's own cancel failing still ends it, as before.
    app.on_message(to_run(id, finished(None, "", true)));
    send(&mut app, "hi");
    app.on_message(started("t1"));
    let refused = Message::CancelFailed {
        task_id: "t1".into(),
        error: "gone".into(),
    };
    app.on_message(refused);
    assert!(!app.activity.working());
}

#[test]
fn a_command_and_a_turn_keep_their_output_apart() {
    let mut app = chat();
    send(&mut app, "hi");
    app.on_message(started("t1"));
    app.on_message(output(OutputStream::Stdout, b"Hel"));
    let (id, _) = run(&mut app, "ls");
    assert_eq!(app.doing(), "writing", "the agent's, not the command's");
    // Both split a character across chunks, interleaved.
    app.on_message(to_run(id, output(OutputStream::Stdout, b"a.txt caf\xc3")));
    app.on_message(output(OutputStream::Stdout, b"lo caf\xc3"));
    app.on_message(to_run(id, output(OutputStream::Stdout, b"\xa9\n")));
    app.on_message(output(OutputStream::Stdout, b"\xa9"));
    app.on_message(to_run(id, finished(Some(0), "", false)));
    assert!(
        app.activity.working(),
        "the command ending isn't the turn's"
    );
    app.on_message(finished(Some(0), "", false));

    let thread: Vec<_> = app
        .thread
        .iter()
        .map(|e| (e.role, e.text.as_str()))
        .take(4)
        .collect();
    assert_eq!(
        thread,
        [
            (Role::User, "hi"),
            (Role::Agent, "Hel"),
            (Role::Command, "ls"),
            (Role::Agent, "lo café"),
        ]
    );
    assert_eq!(printed(block(&app, "ls")), [(false, "a.txt café\n")]);
}
