//! The comments on disk, per workspace: ids, loading and saving.
use super::review_comment::{Comment, Status};
use super::*;
use crate::state::support_dir;
use serde::{Deserialize, Serialize};

/// Ids for comments saved before there were any (they load as 0), counting on from `last`;
/// true if any got one.
fn give_ids(c: &mut [Comment], last: &mut u64) -> bool {
    *last = c.iter().map(|c| c.id).fold(*last, u64::max);
    let fresh = c.iter().any(|c| c.id == 0);
    for c in c.iter_mut().filter(|c| c.id == 0) {
        *last += 1;
        c.id = *last;
    }
    fresh
}

/// Comments that never reached an agent are pending again; true if any was. With `chat`
/// (closed): its queue and the reviews it hadn't read (`unread`). Without (loading, after a
/// restart): every queue, and reviews sent to a chat that is gone.
pub(crate) fn unqueue(l: &mut [Comment], chat: Option<&str>, unread: &[u64]) -> bool {
    let mut any = false;
    for c in l.iter_mut() {
        let to = c.chat.as_deref();
        let lost = match (&c.status, chat) {
            (Status::Queued, None) => true,
            (Status::Queued, Some(id)) => to == Some(id),
            (Status::Sent { .. }, Some(id)) => to == Some(id) && unread.contains(&c.id),
            (Status::Sent { .. }, None) => {
                to.is_some_and(|id| !crate::transcript::transcript_path(id).exists())
            }
            _ => false,
        };
        if lost {
            (c.status, c.chat) = (Status::Pending, None);
            any = true;
        }
    }
    any
}

/// comments/<ws>.json: the comments and the last id given out, so a deleted comment's id
/// isn't reused (old review cards link to it). Older files are the list alone.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Store {
    Ids { last: u64, comments: Vec<Comment> },
    List(Vec<Comment>),
}

pub(crate) fn comments_path(ws: &str) -> PathBuf {
    support_dir().join("comments").join(format!("{ws}.json"))
}

#[derive(Default)]
pub(crate) struct CommentStore {
    /// Comments per workspace, loaded from disk on first use.
    pub(crate) comments: HashMap<String, Vec<Comment>>,
    /// The last comment id given out per workspace.
    last_id: HashMap<String, u64>,
    /// Workspaces whose comments file couldn't be read or moved aside: never saved over.
    unsaved: Vec<String>,
    pub(crate) failed_saves: HashMap<String, String>,
    /// Why, for the window to show once.
    pub(crate) warning: Option<String>,
}

impl CommentStore {
    /// Writes the file whole (a temp file renamed over it), so a crash can't leave half.
    pub(crate) fn save(&mut self, ws: &str) {
        let path = comments_path(ws);
        let result = if self.unsaved.iter().any(|w| w == ws) {
            Err(std::io::Error::other(
                "The unreadable comments store is protected",
            ))
        } else {
            serde_json::to_vec(&Store::Ids {
                last: self.last_id.get(ws).copied().unwrap_or(0),
                comments: self.comments.get(ws).cloned().unwrap_or_default(),
            })
            .map_err(std::io::Error::other)
            .and_then(|json| crate::state::write(&path, &json))
        };
        match result {
            Ok(()) => {
                self.failed_saves.remove(ws);
            }
            Err(e) => {
                self.failed_saves.insert(ws.to_string(), e.to_string());
            }
        }
    }

    pub(crate) fn retry(&mut self) {
        for ws in self.failed_saves.keys().cloned().collect::<Vec<_>>() {
            self.save(&ws);
        }
    }

    /// Loads once; comments saved before ids get their own, ones queued in a chat that is
    /// gone now are pending. A file that doesn't parse is kept aside as `.json.<n>.bak`; one
    /// that can't be read (or moved) is left alone and not saved over.
    pub(crate) fn load(&mut self, ws: &str) {
        if self.comments.contains_key(ws) {
            return;
        }
        let path = comments_path(ws);
        let (store, read) = match std::fs::read(&path) {
            Ok(b) => (
                Some(serde_json::from_slice(&b).map_err(|e| e.to_string())),
                true,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, true),
            Err(e) => (Some(Err(e.to_string())), false),
        };
        let (mut last, mut c) = match store {
            Some(Ok(Store::Ids { last, comments })) => (last, comments),
            Some(Ok(Store::List(c))) => (0, c),
            Some(Err(e)) => {
                let bak = (1..)
                    .map(|n| path.with_extension(format!("json.{n}.bak")))
                    .find(|p| !p.exists())
                    .unwrap();
                let file = path.display();
                self.warning = Some(match read && std::fs::rename(&path, &bak).is_ok() {
                    true => format!("Couldn't read {file} ({e}); moved it to {}", bak.display()),
                    false => {
                        self.unsaved.push(ws.to_string());
                        format!("Couldn't read {file} ({e}); nothing is saved until it's fixed")
                    }
                });
                (0, vec![])
            }
            None => (0, vec![]),
        };
        let fresh = give_ids(&mut c, &mut last) | unqueue(&mut c, None, &[]);
        self.comments.insert(ws.to_string(), c);
        self.last_id.insert(ws.to_string(), last);
        if fresh {
            self.save(ws);
        }
    }

    /// A new comment id in workspace `ws`, never one given out before.
    pub(crate) fn new_id(&mut self, ws: &str) -> u64 {
        self.load(ws);
        let last = self.last_id.entry(ws.to_string()).or_default();
        *last += 1;
        *last
    }

    /// A removed workspace's comments, so a later one of the same name starts with none.
    pub(crate) fn forget(&mut self, ws: &str) {
        self.comments.remove(ws);
        self.last_id.remove(ws);
        self.unsaved.retain(|w| w != ws);
        self.failed_saves.remove(ws);
        let _ = std::fs::remove_file(comments_path(ws));
    }

    /// Edits workspace `ws`'s comments and saves them if that changed them.
    pub(crate) fn edit(&mut self, ws: &str, f: impl FnOnce(&mut Vec<Comment>)) {
        self.load(ws);
        let l = self.comments.get_mut(ws).unwrap();
        let before = l.clone();
        f(l);
        if *l != before || self.failed_saves.contains_key(ws) {
            self.save(ws);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// comments/<ws>.json from before ids and statuses still loads; each gets its own id.
    #[::core::prelude::v1::test]
    fn old_comment_files_load() {
        let old = r#"[{"path":"api/a","start":[false,1],"end":[false,2],"text":"one"},
                      {"path":"api/b","start":[true,3],"end":[true,3],"text":"two"}]"#;
        let c: Vec<Comment> = serde_json::from_str(old).unwrap();
        assert_eq!(
            (c[0].id, &c[0].status, c[1].start),
            (0, &Status::Pending, (true, 3))
        );
        let mut c = c;
        let mut last = 0;
        assert!(give_ids(&mut c, &mut last));
        assert_eq!(c.iter().map(|c| c.id).collect::<Vec<_>>(), [1, 2]);
        assert!(!give_ids(&mut c, &mut last), "only once");
        assert_eq!(last, 2);
        // The list alone (older files) and with the last id both load.
        let Ok(Store::List(l)) = serde_json::from_str(old) else {
            panic!("old list")
        };
        assert_eq!(l.len(), 2);
        let new = r#"{"last":9,"comments":[{"id":3,"path":"api/a","start":[false,1],"end":[false,1],"text":"x"}]}"#;
        let Ok(Store::Ids { last, comments }) = serde_json::from_str(new) else {
            panic!("new store")
        };
        assert_eq!((last, comments[0].id), (9, 3));
        // A comment queued in a chat that is gone is pending again; others stay.
        let mut l = vec![
            Comment {
                id: 1,
                status: Status::Queued,
                chat: Some("a".into()),
                ..Default::default()
            },
            Comment {
                id: 2,
                status: Status::Queued,
                chat: Some("b".into()),
                ..Default::default()
            },
            Comment {
                id: 3,
                status: Status::Sent { turn: 1 },
                chat: Some("a".into()),
                ..Default::default()
            },
        ];
        assert!(unqueue(&mut l, Some("a"), &[]));
        assert_eq!((&l[0].status, &l[0].chat), (&Status::Pending, &None));
        assert_eq!(l[1].status, Status::Queued);
        assert_eq!(
            l[2].status,
            Status::Sent { turn: 1 },
            "read before it closed"
        );
        // Closed before reading it: pending again.
        l[2].status = Status::Sent { turn: 1 };
        assert!(unqueue(&mut l, Some("a"), &[3]));
        assert_eq!((&l[2].status, &l[2].chat), (&Status::Pending, &None));
        // After a restart: sent to a chat that is gone (no transcript), pending again.
        l[2].status = Status::Sent { turn: 1 };
        l[2].chat = Some("no-such-chat-r3".into());
        assert!(unqueue(&mut l, None, &[]) && !unqueue(&mut l, None, &[]));
        assert_eq!(
            (&l[1].status, &l[2].status),
            (&Status::Pending, &Status::Pending)
        );
        let s = Status::Addressed {
            turn: 5,
            note: "n".into(),
        };
        let back: Status = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
