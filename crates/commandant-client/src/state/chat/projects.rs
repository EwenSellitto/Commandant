//! `/project`: the node's projects to browse, join or copy, from a chat.

use commandant_proto::*;

use super::{Activity, Chat};
use crate::state::{Choice, Choose, Effect, Pick};

impl Chat {
    /// `/project`: browse the node's projects, or with a repository (or a
    /// project's name), clone a new copy of it to work in. A session stays in
    /// its directory, so only one that hasn't started can move.
    pub(super) fn project(&mut self, repository: &str) -> Option<Effect> {
        if !self.settings.session_id.is_empty() || matches!(self.activity, Activity::Working { .. })
        {
            self.info(
                "this session already works somewhere: start a new one (Ctrl-N) for a project",
            );
            return None;
        }
        if !repository.is_empty() {
            return Some(self.new_copy(repository.to_string()));
        }
        if self.listing_projects {
            return None;
        }
        self.listing_projects = true;
        Some(Effect::FetchProjects {
            chat: self.id,
            node: self.node.id.clone(),
        })
    }

    pub(super) fn new_copy(&mut self, repository: String) -> Effect {
        self.preparing = Some(repository.clone());
        Effect::PrepareProject {
            chat: self.id,
            node: self.node.id.clone(),
            repository,
        }
    }

    pub(super) fn work_in(&mut self, path: &str, id: &str) {
        self.info(&format!("working in copy {id}: {path}"));
        self.settings.cwd = path.to_string();
    }

    /// The node's projects to browse: a new copy of each, then its copies
    /// with what their sessions are about.
    pub(super) fn open_projects(&mut self, projects: Vec<commandant_proto::Project>) {
        if projects.is_empty() {
            self.info("the node has no projects yet: /project <repository URL> clones one");
            return;
        }
        let mut choices = Vec::new();
        for project in projects {
            let label = format!("+ new copy of {}", project.name);
            let new = Choose::NewCopy(project.name.clone());
            choices.push(Choice::new(new, label, &project.repository));
            for copy in project.copies {
                let label = format!("{} · {}", project.name, copy.id);
                let detail = copy_summary(&copy);
                choices.push(Choice::new(Choose::Join(copy), label, detail));
            }
        }
        self.pick = Some(Pick::new(
            "Projects (join a copy, or make a new one)",
            choices,
            None,
        ));
    }
}

/// `main · fix the parser; add tests +1`: a copy's branch and what its
/// sessions are about.
fn copy_summary(copy: &ProjectCopy) -> String {
    let sessions = match copy.sessions.as_slice() {
        [] => "no sessions yet".to_string(),
        [one] => one.clone(),
        [one, two] => format!("{one}; {two}"),
        [one, two, rest @ ..] => format!("{one}; {two} +{}", rest.len()),
    };
    match copy.branch.as_str() {
        "" => sessions,
        branch => format!("{branch} · {sessions}"),
    }
}
