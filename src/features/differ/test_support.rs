//! Private-state access for the app integration fixtures. Not compiled into the app.
use super::*;

pub(crate) mod diff {
    pub(crate) use super::super::diff::*;
}
pub(crate) mod inline_edit {
    pub(crate) use super::super::inline_edit::*;
}
pub(crate) mod review {
    pub(crate) use super::super::review::*;
}

impl Differ {
    pub(crate) fn collapsed_dirs(&self) -> &HashSet<String> {
        &self.collapsed_dirs
    }
    pub(crate) fn diff(&self) -> &Entity<DiffState> {
        &self.diff
    }
    pub(crate) fn diff_error(&self) -> &Option<String> {
        &self.diff_error
    }
    pub(crate) fn diff_file(&self) -> usize {
        self.diff_file
    }
    pub(crate) fn diff_files(&self) -> &Vec<DiffFile> {
        &self.diff_files
    }
    pub(crate) fn diff_fp(&self) -> u64 {
        self.diff_fp
    }
    pub(crate) fn diff_gen(&self) -> u64 {
        self.diff_gen
    }
    pub(crate) fn diff_loaded(&self) -> bool {
        self.diff_loaded
    }
    pub(crate) fn editing(&self) -> &Option<inline_edit::InlineEdit> {
        &self.editing
    }
    pub(crate) fn media(&self) -> &Vec<(SharedString, Entity<media::MediaView>)> {
        &self.media
    }
    pub(crate) fn menu(&self) -> &Option<(Entity<menu::Menu>, Subscription)> {
        &self.menu
    }
    pub(crate) fn preferences(&self) -> &Preferences {
        &self.preferences
    }
    pub(crate) fn review(&self) -> &review::Review {
        &self.review
    }
    pub(crate) fn show_tree(&self) -> bool {
        self.show_tree
    }
    pub(crate) fn tree_filter(&self) -> &Entity<InputState> {
        &self.tree_filter
    }
    pub(crate) fn tree_focus(&self) -> &FocusHandle {
        &self.tree_focus
    }
    pub(crate) fn tree_query(&self) -> &String {
        &self.tree_query
    }
    pub(crate) fn tree_resize(&self) -> &Entity<ResizableState> {
        &self.tree_resize
    }
    pub(crate) fn tree_rows(&self) -> &Vec<Row> {
        &self.tree_rows
    }
    pub(crate) fn tree_scroll(&self) -> &UniformListScrollHandle {
        &self.tree_scroll
    }
    pub(crate) fn tree_sel(&self) -> usize {
        self.tree_sel
    }
    pub(crate) fn uncommitted(&self) -> &HashSet<String> {
        &self.uncommitted
    }
    pub(crate) fn viewed_now(&self) -> &Rc<HashSet<SharedString>> {
        &self.viewed_now
    }
    pub(crate) fn workspace(&self) -> &Option<Workspace> {
        &self.workspace
    }
    pub(crate) fn diff_error_mut(&mut self) -> &mut Option<String> {
        &mut self.diff_error
    }
    pub(crate) fn diff_file_mut(&mut self) -> &mut usize {
        &mut self.diff_file
    }
    pub(crate) fn diff_files_mut(&mut self) -> &mut Vec<DiffFile> {
        &mut self.diff_files
    }
    pub(crate) fn diff_fp_mut(&mut self) -> &mut u64 {
        &mut self.diff_fp
    }
    pub(crate) fn diff_fps_mut(&mut self) -> &mut Vec<u64> {
        &mut self.diff_fps
    }
    pub(crate) fn diff_gen_mut(&mut self) -> &mut u64 {
        &mut self.diff_gen
    }
    pub(crate) fn diff_loaded_mut(&mut self) -> &mut bool {
        &mut self.diff_loaded
    }
    pub(crate) fn editing_mut(&mut self) -> &mut Option<inline_edit::InlineEdit> {
        &mut self.editing
    }
    pub(crate) fn preferences_mut(&mut self) -> &mut Preferences {
        &mut self.preferences
    }
    pub(crate) fn review_mut(&mut self) -> &mut review::Review {
        &mut self.review
    }
    pub(crate) fn show_tree_mut(&mut self) -> &mut bool {
        &mut self.show_tree
    }
    pub(crate) fn tree_sel_mut(&mut self) -> &mut usize {
        &mut self.tree_sel
    }
    pub(crate) fn uncommitted_mut(&mut self) -> &mut HashSet<String> {
        &mut self.uncommitted
    }
    pub(crate) fn collapsed_dirs_mut(&mut self) -> &mut HashSet<String> {
        &mut self.collapsed_dirs
    }
}
