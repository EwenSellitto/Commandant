use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub name: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub conversations: Vec<Conversation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceBrowserData {
    pub projects: Vec<Project>,
}

impl WorkspaceBrowserData {
    pub fn sample() -> Self {
        Self {
            projects: vec![
                Project {
                    id: "project-commandant".to_string(),
                    name: "Commandant".to_string(),
                    conversations: vec![
                        Conversation {
                            id: "conv-commandant-shell".to_string(),
                            name: "Shell Workflow".to_string(),
                            text: "Simple notes for the shell workflow. Keep terminal actions predictable, prefer small commands, and expose the important state in the interface.".to_string(),
                        },
                        Conversation {
                            id: "conv-commandant-sync".to_string(),
                            name: "Server Sync".to_string(),
                            text: "Conversation history can later be hydrated from the server. For now this text stands in as the preview content for the selected conversation.".to_string(),
                        },
                        Conversation {
                            id: "conv-commandant-design".to_string(),
                            name: "Drawer Design".to_string(),
                            text: "Each project acts like a drawer. The drawer opens to reveal a one-level list of conversations, keeping navigation compact and easy to scan.".to_string(),
                        },
                    ],
                },
                Project {
                    id: "project-atlas".to_string(),
                    name: "Atlas".to_string(),
                    conversations: vec![
                        Conversation {
                            id: "conv-atlas-roadmap".to_string(),
                            name: "Roadmap Review".to_string(),
                            text: "Atlas conversations are displayed as leaf nodes. Selecting one shows its plain text content in the detail pane.".to_string(),
                        },
                        Conversation {
                            id: "conv-atlas-bugs".to_string(),
                            name: "Bug Bash".to_string(),
                            text: "This view is designed to feel like a file browser, but limited to a single project depth so the hierarchy stays focused.".to_string(),
                        },
                    ],
                },
                Project {
                    id: "project-orbit".to_string(),
                    name: "Orbit".to_string(),
                    conversations: vec![
                        Conversation {
                            id: "conv-orbit-planning".to_string(),
                            name: "Planning Session".to_string(),
                            text: "Use the arrow keys to move, Enter or Right to open a drawer, Left to close it, and q to quit the interface.".to_string(),
                        },
                        Conversation {
                            id: "conv-orbit-release".to_string(),
                            name: "Release Notes".to_string(),
                            text: "The left pane is intentionally scrollable so projects with many conversations remain usable even in smaller terminals.".to_string(),
                        },
                        Conversation {
                            id: "conv-orbit-feedback".to_string(),
                            name: "User Feedback".to_string(),
                            text: "A later iteration can replace this sample dataset with data from disk or the server while preserving the same TUI structure.".to_string(),
                        },
                    ],
                },
                Project {
                    id: "project-nomad".to_string(),
                    name: "Nomad".to_string(),
                    conversations: vec![
                        Conversation {
                            id: "conv-nomad-intro".to_string(),
                            name: "Kickoff".to_string(),
                            text: "Nomad is here mostly to make the list long enough to test scrolling behavior in the drawer pane.".to_string(),
                        },
                        Conversation {
                            id: "conv-nomad-copy".to_string(),
                            name: "Copy Ideas".to_string(),
                            text: "The current design uses simple text only, matching the lightweight content model you asked for.".to_string(),
                        },
                        Conversation {
                            id: "conv-nomad-wrap".to_string(),
                            name: "Wrap Up".to_string(),
                            text: "The detail pane wraps content and surfaces the current selection, giving the browser a clear split between navigation and reading.".to_string(),
                        },
                    ],
                },
            ],
        }
    }
}
