// What the pages share: the key hints of the commit graph, the status line, copying,
// and what each page keeps across a reload of the repository.

use std::ops::RangeInclusive;

use ratatui::{style::Style, text::Span};

use crate::{
    browse::{LinkAction, LinkTarget},
    context::AppContext,
    event::AppEvent,
    external::editor_command,
    git::{
        git_bytes, work_path, AfterTask, CommitHash, FileRevision, GitCommand, GitTask,
        PatchDirection, Repository,
    },
    graph_commands::{moves_head, single_commit_command, Compare},
    keybind::UserEvent,
    keys::{Availability, Keys},
    search::{SearchKind, SearchOptions, SearchOutcome},
    view::detail::DetailFocus,
    widget::{
        commit_list::format_date,
        commit_list::{CommitListState, SearchRefreshContext},
        commit_tabs::DetailTab,
        diff_pane::DiffPane,
        file_tree::{Entry, FileTreeRow, ROOT_PATH},
        style::ListCursor,
        tree_query::{highlighted_text, name_matches, TreeQuery},
    },
};

// The commands of the commit graph on the selected commit, or the picked ones, as `availability`
// says of each: in the commit list and the details, and on a ref of the sidebar. While commits
// are `picked`, those of one commit say to clear them, and while an operation waits to be
// finished those moving HEAD say so, unlisted.
pub fn register_graph_commands<T: ?Sized>(
    k: &mut Keys<T>,
    availability: impl Fn(&T, UserEvent) -> Availability + Copy,
    picked: impl Fn(&T) -> bool + Copy,
    operation_waits: impl Fn(&T) -> bool + Copy,
) {
    use UserEvent::*;
    let commands: [(UserEvent, &str, &'static str); 13] = [
        (
            Diff,
            "diff",
            "Diff with the checked-out commit, or the two picked commits or refs",
        ),
        (Checkout, "checkout", "Check out the branch, tag or commit"),
        (
            Merge,
            "merge",
            "Merge the commit or ref into the current branch",
        ),
        (
            Rebase,
            "rebase",
            "Rebase the current branch onto the commit or ref",
        ),
        (
            CherryPick,
            "cherry-pick",
            "Cherry-pick the picked commits, or the selected one",
        ),
        (
            Squash,
            "squash",
            "Squash the picked commits, or the selected one, into their parents",
        ),
        (
            AmendCommit,
            "amend",
            "Amend the commit on the local changes page",
        ),
        (
            DropCommit,
            "drop",
            "Drop the picked commits, or the selected one",
        ),
        (
            CreateBranch,
            "new branch",
            "New branch at the commit or ref",
        ),
        (CreateTag, "new tag", "New tag at the commit or ref"),
        (
            BranchOnly,
            "branch only",
            "Show only the commits of the current (or selected) branch, or all",
        ),
        (
            Revert,
            "revert",
            "Revert the picked commits, or the selected one",
        ),
        (
            Reset,
            "reset",
            "Reset the current branch to the commit or ref",
        ),
    ];
    for (event, short, long) in commands {
        let single = single_commit_command(event).is_some();
        // space checks out, the main action on a commit or ref
        let events: &[UserEvent] = match event {
            Checkout => &[Checkout, Select],
            _ => &[event],
        };
        k.add_where(events, short, long, move |view| {
            let refused = (single && picked(view)) || (moves_head(event) && operation_waits(view));
            match availability(view, event) {
                Availability::Works if refused => Availability::Warns,
                availability => availability,
            }
        });
    }
}

// What y and Y copy: a short form, like a relative path, or the full one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyForm {
    Short,
    Full,
}

impl CopyForm {
    pub fn of(event: UserEvent) -> Self {
        if event == UserEvent::FullCopy {
            CopyForm::Full
        } else {
            CopyForm::Short
        }
    }
}

// Copies a commit hash, short or in full.
pub fn copy_hash(hash: &CommitHash, form: CopyForm) -> AppEvent {
    let (name, value) = match form {
        CopyForm::Full => ("Commit SHA", hash.as_str()),
        CopyForm::Short => ("Commit SHA (short)", hash.as_short_hash()),
    };
    AppEvent::copy(name, value)
}

// Copies several commit hashes, short or in full, one per line.
pub fn copy_hashes(hashes: &[CommitHash], form: CopyForm) -> AppEvent {
    let (name, value): (&str, Vec<&str>) = match form {
        CopyForm::Full => (
            "Commit SHAs",
            hashes.iter().map(CommitHash::as_str).collect(),
        ),
        CopyForm::Short => (
            "Commit SHAs (short)",
            hashes.iter().map(CommitHash::as_short_hash).collect(),
        ),
    };
    AppEvent::copy(name, value.join("\n"))
}

// The keys acting on a commit wherever it is shown: copying its hash, opening its page, checking
// it out and selecting it on the graph.
pub fn commit_event(event: UserEvent, commit: &CommitHash) -> Option<AppEvent> {
    match event {
        UserEvent::ShortCopy | UserEvent::FullCopy => Some(copy_hash(commit, CopyForm::of(event))),
        UserEvent::OpenInBrowser | UserEvent::CopyLink => Some(AppEvent::Link {
            target: LinkTarget::Commit(commit.clone()),
            action: LinkAction::of(event),
        }),
        UserEvent::Checkout => Some(AppEvent::Checkout(commit.clone())),
        UserEvent::Confirm => Some(AppEvent::ShowCommit(commit.clone())),
        _ => None,
    }
}

// Whether a search matches the commit of a file's history, by its hash, date or subject, as the
// commit list searches; in the timeline.
pub fn revision_matches(
    ctx: &AppContext,
    revision: &FileRevision,
    query: &str,
    options: SearchOptions,
) -> bool {
    // the date, formatted for it, looked at last
    name_matches(revision.commit.as_str(), query, options)
        || name_matches(&revision.subject, query, options)
        || name_matches(&format_date(ctx, &revision.date), query, options)
}

// The keys moving in a list of lines, in the blame and the timeline, where `applies`.
pub fn register_moving_keys<T: ?Sized>(k: &mut Keys<T>, applies: impl Fn(&T) -> bool + Copy) {
    use UserEvent::*;
    k.add(&[NavigateDown, NavigateUp], "move", "Move down/up")
        .when(applies);
    k.add(&[GoToTop, GoToBottom], "top/bottom", "Go to the top/bottom")
        .when(applies);
    k.add(
        &[HalfPageDown, HalfPageUp],
        "half page",
        "Jump half a page down/up",
    )
    .when(applies);
    k.add(&[PageDown, PageUp], "page", "Scroll a page down/up")
        .when(applies);
    k.add(&[ScrollDown, ScrollUp], "scroll", "Scroll down/up")
        .when(applies);
}

// Space on the diff of a commit, where `pane` is: applying the selected hunk or lines to the
// working tree, see `apply_selection`.
pub fn register_apply_key<T: ?Sized>(k: &mut Keys<T>, pane: fn(&T) -> Option<&DiffPane>) {
    k.add(
        &[UserEvent::Select],
        "apply to working tree",
        "Apply the selected hunk or line to the working tree",
    )
    .when(move |v| pane(v).is_some());
}

// The keys of `commit_event`, where `applies`.
pub fn register_commit_keys<T: ?Sized>(k: &mut Keys<T>, applies: impl Fn(&T) -> bool + Copy) {
    use UserEvent::*;
    k.add(
        &[Confirm],
        "show on graph",
        "Select the commit on the graph, closing the window",
    )
    .when(applies);
    // space checks out, the main action on a commit
    k.add(&[Checkout, Select], "checkout", "Check out the commit")
        .when(applies);
    register_commit_link_keys(k, applies, applies);
}

// o, O and y on a commit, where `applies`: its page on the web host and its link, where `linked`
// too, and its hash.
pub fn register_commit_link_keys<T: ?Sized>(
    k: &mut Keys<T>,
    applies: impl Fn(&T) -> bool + Copy,
    linked: impl Fn(&T) -> bool + Copy,
) {
    use UserEvent::*;
    k.add(
        &[OpenInBrowser],
        "open in browser",
        "Open the commit on the web host",
    )
    .when(move |v| applies(v) && linked(v));
    k.add(&[CopyLink], "copy link", "Copy the link of the commit")
        .when(move |v| applies(v) && linked(v));
    k.add(
        &[ShortCopy],
        "copy hash",
        "Copy the short hash of the commit",
    )
    .when(applies);
    k.add(
        &[FullCopy],
        "copy full hash",
        "Copy the full hash of the commit",
    )
    .when(applies);
}

// o and O on a file at a commit, at the selected lines: its page on the web host, or its link.
pub fn file_link(
    event: UserEvent,
    commit: &CommitHash,
    path: &str,
    lines: Option<RangeInclusive<usize>>,
) -> AppEvent {
    let target = LinkTarget::File {
        commit: commit.clone(),
        path: path.into(),
        lines,
    };
    AppEvent::Link {
        target,
        action: LinkAction::of(event),
    }
}

// e on files of the repository: opens them as they are in the working tree in the editor, or says
// which is gone from it.
pub fn edit_files(repository: &Repository, files: &[&str]) -> AppEvent {
    match repository.working_tree_paths(files) {
        Ok(paths) => AppEvent::RunExternal(editor_command(&paths)),
        Err(err) => AppEvent::NotifyError(err),
    }
}

// The hash of a commit is cut to this many characters in the blame and the timeline.
pub const SHORT_HASH_WIDTH: usize = 7;

// A commit changing a file, as the blame and the timeline show it: its hash, date and subject,
// with what `search` matches in them highlighted.
pub fn revision_spans(
    ctx: &AppContext,
    revision: &FileRevision,
    search: Option<&str>,
) -> Vec<Span<'static>> {
    let theme = &ctx.color_theme;
    let options = ctx.search_options.get();
    let text = |text: &str, shown: usize, fg| {
        highlighted_text(text, shown, Style::default().fg(fg), search, options, theme)
    };
    let date = format_date(ctx, &revision.date);
    let mut spans = text(
        revision.commit.as_str(),
        SHORT_HASH_WIDTH,
        theme.list_hash_fg,
    );
    spans.push(Span::raw(" "));
    spans.extend(text(&date, date.len(), theme.list_date_fg));
    spans.push(Span::raw(" "));
    spans.extend(text(
        &revision.subject,
        revision.subject.len(),
        theme.list_subject_fg,
    ));
    spans
}

// The status line showing the `query` being typed, a search of `kind`, with its cursor and its
// options on the right; cleared when nothing is typed.
// What the status line says of `query`, searching `kind`: the query while it is typed, else how
// its search went, the `outcome`.
pub fn query_or_search_status(
    ctx: &AppContext,
    query: &TreeQuery,
    kind: SearchKind,
    outcome: impl FnOnce() -> Option<SearchOutcome>,
) -> AppEvent {
    match query.is_editing() {
        true => query_status(ctx, query, kind),
        false => search_status(kind, ctx.search_options.get(), outcome()),
    }
}

pub fn query_status(ctx: &AppContext, query: &TreeQuery, kind: SearchKind) -> AppEvent {
    match query.status_input() {
        Some((message, cursor_byte_index)) => AppEvent::UpdateStatusInput {
            message,
            cursor_byte_index,
            metadata: ctx.search_options.get().status_string(kind),
        },
        None => AppEvent::ClearStatusLine,
    }
}

// The status line telling how a search of `kind` went, alike on every page; the line is cleared
// when there is none.
pub fn search_status(
    kind: SearchKind,
    options: SearchOptions,
    outcome: Option<SearchOutcome>,
) -> AppEvent {
    match outcome {
        Some(outcome) => AppEvent::UpdateSearchResult {
            outcome,
            options: options.status_string(kind),
        },
        None => AppEvent::ClearStatusLine,
    }
}

// Copies the selected part of a diff or a file.
// `name` is the pane's, see `DiffPane::selected_name`.
pub fn copy_selection(text: String, name: &str) -> AppEvent {
    let mut chars = name.chars();
    let name: String = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    AppEvent::copy(&name, text)
}

// Applies the selected hunk or lines of a commit's diff to the working tree.
pub fn apply_selection(pane: &DiffPane) -> Option<AppEvent> {
    let patch = pane.selected_patch(PatchDirection::Forward)?;
    let what = pane.selected_name();
    let task = GitTask {
        commands: vec![GitCommand::Apply.args(["-"])],
        input: Some(patch),
        running: format!("Applying the {what} to the working tree"),
        done: format!("Applied the {what} to the working tree"),
        // git names the lines that differ, which tells little more
        failed: Some(format!(
            "The {what} doesn't apply to the working tree, the file differs there"
        )),
        after: AfterTask::KeepView,
        ..Default::default()
    };
    Some(AppEvent::StartTask(task))
}

// The value `copy_path` and `copy_paths` copy: relative to the top of the repository, or absolute
// in full.
fn path_value(repository: &Repository, relative: &str, form: CopyForm) -> String {
    let relative = if relative == ROOT_PATH { "." } else { relative };
    match form {
        CopyForm::Full => {
            let absolute = work_path(repository.path(), relative);
            let absolute = absolute.to_string_lossy();
            absolute.trim_end_matches("/.").to_string()
        }
        // a byte that isn't UTF-8 copied as U+FFFD, see `text_from_git`
        CopyForm::Short => String::from_utf8_lossy(&git_bytes(relative)).into_owned(),
    }
}

// Copies the path of a file in the repository: relative to its top, or absolute in full.
pub fn copy_path(repository: &Repository, row: &FileTreeRow, form: CopyForm) -> AppEvent {
    // the root row stands for the whole repository
    copy_paths(repository, [row.entry().map_or(".", Entry::path)], form)
}

// Copies the paths of several files, one per line: relative to the top, or absolute in full.
pub fn copy_paths<'a>(
    repository: &Repository,
    paths: impl IntoIterator<Item = &'a str>,
    form: CopyForm,
) -> AppEvent {
    let paths: Vec<&str> = paths.into_iter().collect();
    let name = match (form, paths.len()) {
        (CopyForm::Full, 1) => "Absolute Path",
        (CopyForm::Full, _) => "Absolute Paths",
        (CopyForm::Short, 1) => "Path",
        (CopyForm::Short, _) => "Paths",
    };
    let value: Vec<String> = paths
        .into_iter()
        .map(|path| path_value(repository, path, form))
        .collect();
    AppEvent::copy(name, value.join("\n"))
}

#[derive(Debug, Clone)]
pub enum RefreshViewContext {
    List {
        list_context: ListRefreshViewContext,
    },
    Detail {
        list_context: ListRefreshViewContext,
        // the tab shown
        tab: DetailTab,
        // the comparison shown, opened again once reloaded
        compare: Option<Compare>,
        // what had the focus, which reloading the page puts back
        focus: DetailFocus,
    },
    LocalChanges {
        list_context: ListRefreshViewContext,
        local_changes_context: LocalChangesRefreshViewContext,
    },
}

impl RefreshViewContext {
    pub fn list_context_mut(&mut self) -> &mut ListRefreshViewContext {
        match self {
            RefreshViewContext::List { list_context }
            | RefreshViewContext::Detail { list_context, .. }
            | RefreshViewContext::LocalChanges { list_context, .. } => list_context,
        }
    }

    pub fn list_context(&self) -> &ListRefreshViewContext {
        match self {
            RefreshViewContext::List { list_context }
            | RefreshViewContext::Detail { list_context, .. }
            | RefreshViewContext::LocalChanges { list_context, .. } => list_context,
        }
    }
}

// The commit selected once the commit list is reloaded.
#[derive(Debug, Clone)]
pub enum Reselect {
    // the newest commit, the commits made meanwhile showing above it
    Newest,
    Commit(CommitHash),
}

#[derive(Debug, Clone)]
pub struct ListRefreshViewContext {
    pub select: Reselect,
    pub picked_commit_hashes: Vec<CommitHash>,
    // the selected commit's row and the commits shown, restored around it once reloaded
    pub cursor: ListCursor,
    pub search_options: SearchOptions,
    pub search_context: Option<SearchRefreshContext>,
}

impl From<&CommitListState<'_>> for ListRefreshViewContext {
    fn from(list_state: &CommitListState) -> Self {
        let cursor = list_state.cursor();
        let select = match cursor.selected() {
            0 => Reselect::Newest,
            _ => Reselect::Commit(list_state.selected_commit_hash().clone()),
        };
        let search_options = list_state.search_options();
        let search_context = list_state.search_refresh_context();
        ListRefreshViewContext {
            select,
            picked_commit_hashes: list_state.picked_commit_hashes().to_vec(),
            cursor,
            search_options,
            search_context,
        }
    }
}

pub use crate::widget::local_changes::LocalChangesSnapshot as LocalChangesRefreshViewContext;

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{git::SortCommit, test_support::git::GitRepository};

    #[test]
    fn test_copy_paths_joins_one_per_line() {
        let dir = tempfile::tempdir().unwrap();
        let git = GitRepository::new(dir.path());
        git.init();
        for name in ["a", "b"] {
            fs::write(dir.path().join(name), "").unwrap();
        }
        git.add(&["a", "b"]);
        git.commit("init", "2024-01-01");
        let repository =
            Repository::load(dir.path(), SortCommit::Chronological, None, false).unwrap();

        let short = copy_paths(&repository, ["a", "b"], CopyForm::Short);
        let AppEvent::CopyToClipboard { name, value } = short else {
            panic!("not a copy event");
        };
        assert_eq!(name, "Paths");
        assert_eq!(value, "a\nb");

        let full = copy_paths(&repository, ["a", "b"], CopyForm::Full);
        let AppEvent::CopyToClipboard { name, value } = full else {
            panic!("not a copy event");
        };
        assert_eq!(name, "Absolute Paths");
        assert_eq!(
            value,
            format!(
                "{}\n{}",
                dir.path().join("a").display(),
                dir.path().join("b").display()
            )
        );
    }
}
