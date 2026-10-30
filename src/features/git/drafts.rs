//! Git draft types and their debounced, generation-ordered persistence.
use super::GitFeature;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct DescriptionDraft {
    pub(super) original_title: String,
    pub(super) original_body: String,
    pub(super) title: String,
    pub(super) body: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct Draft {
    pub(super) description: Option<DescriptionDraft>,
    pub(super) note: String,
    pub(super) replies: HashMap<String, String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Editing {
    Description,
    Comment,
    Reply(String),
}

impl Draft {
    /// Whether this draft holds text for `kind`.
    pub(super) fn has(&self, kind: &Editing) -> bool {
        match kind {
            Editing::Description => self.description.is_some(),
            Editing::Comment => !self.note.is_empty(),
            Editing::Reply(id) => self.replies.get(id).is_some_and(|body| !body.is_empty()),
        }
    }

    /// Drops the text for `kind`.
    pub(super) fn clear(&mut self, kind: &Editing) {
        match kind {
            Editing::Description => self.description = None,
            Editing::Comment => self.note.clear(),
            Editing::Reply(id) => {
                self.replies.remove(id);
            }
        }
    }

    /// Stores the composer text for `kind`; a description keeps its baseline.
    pub(super) fn set(&mut self, kind: &Editing, title: String, body: String) {
        match kind {
            Editing::Description => {
                if let Some(d) = &mut self.description {
                    d.title = title;
                    d.body = body;
                }
            }
            Editing::Comment => self.note = body,
            Editing::Reply(id) => {
                self.replies.insert(id.clone(), body);
            }
        }
    }
}

pub(super) struct DraftStore {
    pub(super) drafts: HashMap<String, Draft>,
    pub(super) error: Option<String>,
    blocked: bool,
    generation: u64,
    saved: u64,
    #[cfg(test)]
    pub(super) delay: Duration,
    #[cfg(test)]
    pub(super) started: u64,
    task: Option<Task<()>>,
}

impl DraftStore {
    /// Loads the workspace's drafts; an unreadable file blocks saving so it is never overwritten.
    pub(super) fn load(workspace: &str) -> Self {
        let (drafts, error, blocked) = match std::fs::read(store_path(workspace)) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(drafts) => (drafts, None, false),
                Err(e) => (
                    HashMap::new(),
                    Some(format!(
                        "Cannot read Git drafts: {e}. The original file is protected."
                    )),
                    true,
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (HashMap::new(), None, false),
            Err(e) => (
                HashMap::new(),
                Some(format!("Cannot read Git drafts: {e}")),
                true,
            ),
        };
        Self {
            drafts,
            error,
            blocked,
            generation: 0,
            saved: 0,
            #[cfg(test)]
            delay: Duration::ZERO,
            #[cfg(test)]
            started: 0,
            task: None,
        }
    }

    pub(super) fn saving(&self) -> bool {
        self.task.is_some()
    }

    /// Marks the drafts changed and schedules a save.
    pub(super) fn dirty(&mut self, cx: &mut Context<GitFeature>) {
        self.generation += 1;
        self.schedule_save(cx);
    }

    pub(super) fn schedule_save(&mut self, cx: &mut Context<GitFeature>) {
        if self.blocked
            || self.task.is_some()
            || (self.saved == self.generation && self.error.is_none())
        {
            return;
        }
        // One worker owns write ordering; input only updates the next snapshot.
        self.task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(300))
                    .await;
                let Ok((generation, path, bytes)) = this.update(cx, |s, _| {
                    #[cfg(test)]
                    {
                        s.store.started = s.store.generation;
                    }
                    (
                        s.store.generation,
                        store_path(&s.workspace.name),
                        serde_json::to_vec(&s.store.drafts).expect("Git drafts serialize"),
                    )
                }) else {
                    return;
                };
                #[cfg(test)]
                {
                    let delay = this
                        .update(cx, |s, _| std::mem::take(&mut s.store.delay))
                        .unwrap_or_default();
                    cx.background_executor().timer(delay).await;
                }
                let result = cx
                    .background_executor()
                    .spawn(async move { crate::state::write(&path, &bytes) })
                    .await;
                let more = this
                    .update(cx, |s, cx| {
                        let store = &mut s.store;
                        if result.is_ok() {
                            store.saved = generation;
                        }
                        let more = store.generation != generation;
                        if !more {
                            store.error = result.err().map(|e| e.to_string());
                            store.task = None;
                        }
                        cx.notify();
                        more
                    })
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
        }));
    }
}

fn store_path(workspace: &str) -> PathBuf {
    crate::state::support_dir()
        .join("git-drafts")
        .join(format!("{workspace}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn drafts_preserve_baseline_and_separate_targets_and_threads() {
        let mut drafts = HashMap::<String, Draft>::new();
        let d = drafts
            .entry("https://github.com/o/r/pull/1".into())
            .or_default();
        d.description = Some(DescriptionDraft {
            original_title: "old".into(),
            original_body: "baseline".into(),
            title: "new".into(),
            body: "edited".into(),
        });
        d.replies.insert("thread-a".into(), "first".into());
        d.replies.insert("thread-b".into(), "second".into());
        drafts
            .entry("https://github.com/o/r/pull/2".into())
            .or_default()
            .note = "other PR".into();
        let bytes = serde_json::to_vec(&drafts).unwrap();
        let loaded: HashMap<String, Draft> = serde_json::from_slice(&bytes).unwrap();
        let first = &loaded["https://github.com/o/r/pull/1"];
        assert_eq!(
            first.description.as_ref().unwrap().original_body,
            "baseline"
        );
        assert_eq!(first.replies["thread-b"], "second");
        assert_eq!(loaded["https://github.com/o/r/pull/2"].note, "other PR");
    }

    #[::core::prelude::v1::test]
    fn draft_has_set_and_clear_follow_the_editing_kind() {
        let reply = Editing::Reply("t".into());
        let mut d = Draft::default();
        d.set(&Editing::Description, "t".into(), "b".into());
        assert!(!d.has(&Editing::Description), "no baseline, nothing to set");
        d.description = Some(DescriptionDraft::default());
        d.set(&Editing::Description, "t".into(), "b".into());
        d.set(&Editing::Comment, String::new(), "note".into());
        d.set(&reply, String::new(), "r".into());
        assert_eq!(d.description.as_ref().unwrap().body, "b");
        assert!(d.has(&Editing::Description) && d.has(&Editing::Comment) && d.has(&reply));
        d.clear(&Editing::Comment);
        d.clear(&reply);
        d.clear(&Editing::Description);
        assert!(!d.has(&Editing::Description) && !d.has(&Editing::Comment) && !d.has(&reply));
        assert_eq!(d, Draft::default());
    }
}
