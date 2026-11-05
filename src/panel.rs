use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use ratatui::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use serde::{Deserialize, Serialize};

use crate::{
    app::{App, Mode},
    model::{Comment, CommentStore, CommitPick, Scope},
    run_review::RunReview,
};

pub(crate) struct Session {
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Draft {
    editing: Option<usize>,
    text: String,
    caret: usize,
    anchor: Option<Comment>,
}

#[derive(Deserialize)]
pub(crate) struct Saved {
    repo: PathBuf,
    pub scope: Scope,
    pub base: Option<String>,
    pub commit_pick: Option<CommitPick>,
    store: CommentStore,
    run_review: RunReview,
    draft: Option<Draft>,
}

impl Saved {
    pub(crate) fn restore(self, app: &mut App) {
        app.store = self.store;
        app.run_review = self.run_review;
        if let Some(draft) = self.draft {
            app.panel_draft_anchor = draft.anchor;
            app.input = draft.text;
            app.caret = draft.caret.min(app.input.chars().count());
            app.mode = Mode::Composing { editing: draft.editing };
        }
        app.refresh_rendered();
    }
}

impl Session {
    pub(crate) fn environment() -> Option<Self> {
        std::env::var_os("HERDR_REVIEWR_PANEL_STATE").map(|path| Self { path: path.into() })
    }

    pub(crate) fn load(&self, repo: &Path) -> Result<Option<Saved>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let saved: Saved =
            serde_json::from_slice(&bytes).context("Cannot restore Git panel notes")?;
        if saved.repo != repo {
            bail!("Git panel notes belong to a different worktree");
        }
        if saved
            .draft
            .as_ref()
            .and_then(|draft| draft.editing)
            .is_some_and(|index| saved.store.get(index).is_none())
        {
            bail!("Git panel draft refers to a missing comment");
        }
        Ok(Some(saved))
    }

    pub(crate) fn consumed(&self) -> Result<()> {
        std::fs::remove_file(&self.path)?;
        Ok(())
    }

    fn save(&self, app: &App, request: &str) -> Result<()> {
        let draft = if let Mode::Composing { editing } = app.mode {
            Some(Draft {
                editing,
                text: app.input.clone(),
                caret: app.caret,
                anchor: if editing.is_none() { app.build_comment(app.input.clone()) } else { None },
            })
        } else {
            None
        };
        if draft.as_ref().is_some_and(|draft| {
            draft.editing.is_none() && draft.anchor.is_none() && !draft.text.is_empty()
        }) {
            bail!("Cannot save the unfinished comment's anchor");
        }
        let value = serde_json::json!({"request":request,"repo":app.repo,"scope":app.scope,
            "base":app.base,"commit_pick":app.commit_pick,"store":app.store,
            "run_review":app.run_review,"draft":draft});
        let parent = self.path.parent().context("Git panel state has no parent directory")?;
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        staged.write_all(&serde_json::to_vec(&value)?)?;
        staged.persist(&self.path)?;
        Ok(())
    }
}

pub(crate) fn checkpoint_event(app: &mut App, event: &Event) -> bool {
    let Event::Key(key) = event else { return false };
    if key.code != KeyCode::F(12)
        || key.kind != KeyEventKind::Press
        || !key.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return false;
    }
    let Some(session) = Session::environment() else { return false };
    let result = (|| -> Result<()> {
        let request = std::fs::read_to_string(session.path.with_extension("request"))?;
        session.save(app, &request)
    })();
    if let Err(error) = result {
        app.status = format!("Cannot hide Git panel: {error}");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_restores_comments_and_an_unfinished_anchored_draft() {
        let root = tempfile::tempdir().unwrap();
        let session = Session { path: root.path().join("notes.json") };
        let mut app = App::new(root.path().into(), Scope::Branch, Some("main".into()));
        let anchor = Comment {
            file: "x.rs".into(),
            side: crate::model::Side::New,
            start: 3,
            end: 3,
            lines: "+before".into(),
            text: "saved".into(),
            diff_anchored: true,
            rev: crate::model::Rev::Worktree,
        };
        app.store.add(anchor.clone());
        app.panel_draft_anchor = Some(anchor);
        app.input = "unfinished note".into();
        app.caret = 4;
        app.mode = Mode::Composing { editing: None };
        session.save(&app, "ack").unwrap();
        let mut restored = App::new(root.path().into(), Scope::Uncommitted, None);
        session.load(root.path()).unwrap().unwrap().restore(&mut restored);
        assert_eq!(restored.store.get(0).unwrap().text, "saved");
        assert_eq!(restored.input, "unfinished note");
        assert_eq!(restored.caret, 4);
        assert_eq!(restored.pending_location().as_deref(), Some("x.rs:3"));
        restored.submit_comment();
        let comment = restored.store.get(1).unwrap();
        assert_eq!(comment.text, "unfinished note");
        assert_eq!(comment.lines, "+before");
        assert_eq!(comment.start, 3);
        assert!(session.load(Path::new("/different-worktree")).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&session.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        session.consumed().unwrap();
        assert!(session.load(root.path()).unwrap().is_none());
    }
}
