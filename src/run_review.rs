use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read as _;

use anyhow::{Result, bail};
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::{
    app::App,
    git,
    herdr::{RunAgent, RunFailure},
    model::Scope,
    world::DiffEnds,
};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Revision {
    head: Option<String>,
    scope: Scope,
    pub(crate) ends: Option<DiffEnds>,
    content: Option<Vec<u8>>,
}

impl Revision {
    pub fn capture(app: &App, file: &str) -> Result<Self> {
        let content = match std::fs::File::open(app.repo.join(file)) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(crate::diff::MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
                if bytes.len() > crate::diff::MAX_BYTES {
                    bail!("reviewed file exceeds the size limit");
                }
                Some(bytes)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            head: git::head_oid(&app.repo),
            scope: app.scope,
            ends: crate::world::build_changed(&app.world_input())?.changeset.ends,
            content,
        })
    }
}

#[derive(Debug)]
struct Preview {
    agents: Vec<RunAgent>,
    selected: usize,
    text: String,
    scroll: u16,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct RunReview {
    pub preferred_agent: Option<String>,
    revisions: HashMap<u64, Revision>,
    pub(crate) draft_revision: Option<Revision>,
    #[serde(skip)]
    preview: Option<Preview>,
    uncertain: bool,
}

impl RunReview {
    pub(crate) fn record(&mut self, id: u64, revision: Revision) {
        self.revisions.insert(id, revision);
    }

    pub(crate) fn retain(&mut self, store: &crate::model::CommentStore) {
        self.revisions.retain(|id, _| store.index_of(*id).is_some());
    }

    pub fn is_open(&self) -> bool {
        self.preview.is_some()
    }
    pub fn close(&mut self) {
        self.preview = None;
    }
    pub fn scroll(&mut self, delta: i16) {
        if let Some(preview) = &mut self.preview {
            preview.scroll = preview.scroll.saturating_add_signed(delta);
        }
    }
    pub fn move_target(&mut self, delta: isize) {
        if let Some(preview) = &mut self.preview {
            preview.selected = preview
                .selected
                .saturating_add_signed(delta)
                .min(preview.agents.len().saturating_sub(1));
        }
    }

    fn validate(&self, app: &App) -> Result<()> {
        if self.uncertain {
            bail!(
                "delivery uncertain; inspect the agent. This batch cannot be submitted again in this pane"
            );
        }
        for (index, comment) in app.store.iter().enumerate() {
            let saved = app.store.id(index).and_then(|id| self.revisions.get(&id));
            let current = Revision::capture(app, &comment.file)?;
            if saved != Some(&current) {
                bail!(
                    "stale review at {}; refresh, review and recreate the comment",
                    comment.location()
                );
            }
        }
        Ok(())
    }
}

impl App {
    pub fn prepare_run_review(&mut self) {
        if self.store.is_empty() {
            self.status = "no comments yet".into();
            return;
        }
        if let Err(error) = self.run_review.validate(self) {
            self.status = error.to_string();
            return;
        }
        let agents = match RunAgent::candidates(&self.repo) {
            Ok(agents) if !agents.is_empty() => agents,
            Ok(_) => {
                self.status = "no idle agent in this worktree".into();
                return;
            }
            Err(error) => {
                self.status = format!("agent lookup failed: {error}");
                return;
            }
        };
        let selected = agents
            .iter()
            .position(|a| Some(&a.pane_id) == self.run_review.preferred_agent.as_ref())
            .unwrap_or(0);
        let comments: Vec<_> = self.store.iter().collect();
        let prefix = self.plugin_config().and_then(crate::config::PluginConfig::review_prefix);
        let review = crate::export::format_review(prefix, &self.repo, &comments);
        let text = format!(
            "Address the following review comments in this worktree and verify your changes.\nReviewed HEAD: {}\nScope: {}\n\n{}",
            git::head_oid(&self.repo).unwrap_or_else(|| "unborn".into()),
            self.scope.label(),
            review
        );
        self.run_review.preview = Some(Preview { agents, selected, text, scroll: 0 });
    }

    pub fn confirm_run_review(&mut self) {
        if let Err(error) = self.run_review.validate(self) {
            self.run_review.close();
            self.status = error.to_string();
            return;
        }
        let Some(preview) = self.run_review.preview.take() else { return };
        let agent = &preview.agents[preview.selected];
        match agent.submit(&self.repo, &preview.text) {
            Ok(()) => {
                let count = self.store.len();
                self.store.take_all();
                self.close_list();
                self.refresh_rendered();
                self.run_review.revisions.clear();
                self.last_sent_pane = Some(agent.pane_id.clone());
                self.status = format!("submitted {count} comments to {}", agent.label());
            }
            Err(RunFailure::NotSent(why)) => self.status = format!("not submitted: {why}"),
            Err(RunFailure::Uncertain) => {
                self.run_review.uncertain = true;
                self.status = "delivery uncertain; inspect the agent. Comments retained; repeat submission disabled".into();
            }
        }
    }
}

pub fn render(frame: &mut Frame, app: &App) {
    let Some(preview) = &app.run_review.preview else { return };
    let area = frame.area();
    let rect = Rect::new(area.x, area.y, area.width, area.height);
    let mut text = String::from(
        "Enter submits into the selected agent input. Esc cancels. Up/Down selects target. PgUp/PgDn scrolls.\nAny existing agent draft will be included in submission.\n\n",
    );
    for (index, agent) in preview.agents.iter().enumerate() {
        let _ = writeln!(
            text,
            "{} {} ({})",
            if index == preview.selected { ">" } else { " " },
            agent.label(),
            agent.pane_id
        );
    }
    text.push('\n');
    text.push_str(&preview.text);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("Send and run review"))
            .scroll((preview.scroll, 0)),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::Focus,
        model::{Comment, Rev, Side},
    };

    fn fixture() -> (tempfile::TempDir, App) {
        let (dir, git) = crate::test_support::test_repo();
        std::fs::write(dir.path().join("a.rs"), "old\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        std::fs::write(dir.path().join("a.rs"), "new\n").unwrap();
        let mut app = App::new(dir.path().to_path_buf(), Scope::Uncommitted, None);
        app.reload().unwrap();
        app.focus = Focus::Diff;
        app.start_comment();
        app.input_paste("fix this");
        app.submit_comment();
        assert_eq!(app.store.len(), 1);
        (dir, app)
    }

    #[test]
    fn authored_comment_is_valid_until_content_scope_or_head_changes() {
        let (dir, mut app) = fixture();
        app.run_review.validate(&app).unwrap();
        std::fs::write(dir.path().join("a.rs"), "shifted\nnew\n").unwrap();
        assert!(app.run_review.validate(&app).unwrap_err().to_string().contains("stale"));
        assert_eq!(app.store.len(), 1);
        std::fs::write(dir.path().join("a.rs"), "new\n").unwrap();
        app.scope = Scope::Branch;
        assert!(app.run_review.validate(&app).is_err());
        app.scope = Scope::Uncommitted;
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["commit", "--allow-empty", "-qm", "new head"])
            .output()
            .unwrap();
        assert!(app.run_review.validate(&app).is_err());
    }

    #[test]
    fn changing_content_while_composing_cannot_be_submitted() {
        let (dir, mut app) = fixture();
        app.store.take_all();
        app.start_comment();
        std::fs::write(dir.path().join("a.rs"), "edited while typing\n").unwrap();
        app.input_paste("fix this too");
        app.submit_comment();
        assert!(app.run_review.validate(&app).is_err());
        assert_eq!(app.store.len(), 1);
    }

    #[test]
    fn comments_without_proven_author_revision_cannot_be_submitted() {
        let (_dir, mut app) = fixture();
        app.store.add(Comment {
            file: "a.rs".into(),
            side: Side::New,
            start: 1,
            end: 1,
            lines: "+new".into(),
            text: "unproven".into(),
            diff_anchored: true,
            rev: Rev::Worktree,
        });
        assert!(app.run_review.validate(&app).is_err());
        assert_eq!(app.store.len(), 2);
    }

    #[test]
    fn a_file_that_grows_past_the_review_limit_cannot_be_submitted() {
        let (dir, mut app) = fixture();
        std::fs::write(dir.path().join("a.rs"), vec![b'x'; crate::diff::MAX_BYTES + 1]).unwrap();
        app.prepare_run_review();
        assert_eq!(app.store.len(), 1);
        assert!(!app.run_review.is_open());
        assert!(app.status.contains("size limit"));
    }

    #[test]
    fn uncertain_delivery_preserves_review_and_blocks_repeat_submission() {
        let (_dir, mut app) = fixture();
        app.run_review.uncertain = true;
        app.prepare_run_review();
        assert!(!app.run_review.is_open());
        assert_eq!(app.store.len(), 1);
        assert!(app.status.contains("delivery uncertain"));
    }

    #[test]
    fn a_saved_review_keeps_stale_checks_and_uncertain_delivery() {
        let (dir, mut app) = fixture();
        let encoded = serde_json::to_vec(&app.run_review).unwrap();
        app.run_review = serde_json::from_slice(&encoded).unwrap();
        app.run_review.validate(&app).unwrap();
        std::fs::write(dir.path().join("a.rs"), "changed while hidden\n").unwrap();
        assert!(app.run_review.validate(&app).unwrap_err().to_string().contains("stale"));
        app.run_review.uncertain = true;
        let encoded = serde_json::to_vec(&app.run_review).unwrap();
        app.run_review = serde_json::from_slice(&encoded).unwrap();
        app.prepare_run_review();
        assert!(app.status.contains("delivery uncertain"));
        assert_eq!(app.store.len(), 1);
    }
}
