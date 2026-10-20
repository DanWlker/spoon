// What the pages share: the key hints of the commit graph and of typing, the status line, copying,
// and what each page keeps across a reload of the repository.

use std::ops::RangeInclusive;

use ratatui::{crossterm::event::KeyEvent, style::Style, text::Span};

use crate::{
    app::AppContext,
    browse::{LinkAction, LinkTarget},
    event::{AppEvent, UserEvent},
    git::{CommitHash, FileRevision, Repository, Rev},
    keybind::Scope,
    keys::{Availability, KeyList, Keys},
    search::{SearchKind, SearchOptions},
    view::detail::DetailFocus,
    widget::{
        commit_list::format_date,
        commit_list::{CommitListState, SearchOutcome, SearchRefreshContext},
        commit_tabs::DetailTab,
        diff::DiffPart,
        diff_pane::DiffPane,
        file_tree::{Entry, FileTreeRow, ROOT_PATH},
        style::ListCursor,
        tree_query::{highlighted_text, name_matches, QueryKind, TreeQuery},
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
        let single = matches!(
            event,
            Rebase | Merge | Checkout | AmendCommit | Reset | CreateBranch | CreateTag
        );
        k.add_where(&[event], short, long, move |view| {
            let refused = (single && picked(view)) || (event.moves_head() && operation_waits(view));
            match availability(view, event) {
                Availability::Works if refused => Availability::Warns,
                availability => availability,
            }
        });
    }
}

// The keys while typing a search or filter, of what `kind` is searched.
pub fn register_typing_keys(k: &mut Keys<SearchKind>) {
    use UserEvent::*;
    k.add(
        &[SearchTargetToggle],
        "target",
        "Toggle what the search of the commit list looks in",
    )
    .when(|kind| kind.has_target());
    k.add(
        &[SmartCaseToggle],
        "smart case",
        "Toggle smart case or case-sensitive, for every search",
    );
    k.add(
        &[FuzzyToggle],
        "fuzzy",
        "Toggle fuzzy match, for every search but of text",
    )
    .when(|kind| kind.has_fuzzy());
    k.add(&[Confirm], "apply", "Keep the query");
    k.add(&[Cancel], "cancel", "Clear the query");
}

pub fn typing_keys(kind: SearchKind) -> KeyList {
    let mut keys = Keys::of(&kind);
    register_typing_keys(&mut keys);
    keys.list()
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
    let date = format_date(ctx, &revision.date);
    [revision.commit.as_str(), &date, &revision.subject]
        .iter()
        .any(|text| name_matches(text, query, options))
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

// The keys of a diff, alike on every page, where `pane` gives it while it has the focus: into the
// lines of the selected hunk, and copying the hunk or line.
pub fn register_diff_keys<T: ?Sized>(
    k: &mut Keys<T>,
    pane: fn(&T) -> Option<&DiffPane>,
    context: fn(&T) -> usize,
) {
    use UserEvent::*;
    k.add(
        &[Confirm],
        "select lines",
        "Select the hunks of the diff, then their lines",
    )
    .when(|v| pane(v).is_some_and(|p| !p.is_selecting_lines()));
    k.add(&[ShortCopy], "copy hunk", "Copy the selected hunk or line")
        .when(|v| pane(v).is_some())
        .short(|v| {
            let part = pane(v).map_or(DiffPart::Hunk, DiffPane::selected_part);
            format!("copy {}", part.name())
        });
    // a diff of several files has no one file to show more or less context of
    let single_file = |v: &T| pane(v).is_some_and(DiffPane::is_single_file);
    k.add(
        &[IncreaseDiffContext],
        "more context",
        "Show more lines around the changes",
    )
    .when(single_file);
    k.add(
        &[DecreaseDiffContext],
        "less context",
        "Show fewer lines around the changes",
    )
    .when(move |v| single_file(v) && context(v) > 0);
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
    k.add(&[Checkout], "checkout", "Check out the commit")
        .when(applies);
    register_commit_link_keys(k, applies);
}

// o, O and y on a commit, where `applies`: its page on the web host, its link and its hash.
pub fn register_commit_link_keys<T: ?Sized>(k: &mut Keys<T>, applies: impl Fn(&T) -> bool + Copy) {
    use UserEvent::*;
    k.add(
        &[OpenInBrowser],
        "open in browser",
        "Open the commit on the web host",
    )
    .when(applies);
    k.add(&[CopyLink], "copy link", "Copy the link of the commit")
        .when(applies);
    k.add(
        &[ShortCopy, FullCopy],
        "copy hash",
        "Copy the commit short/full hash",
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

// The options toggled while typing a search: smart case, and fuzzy matching and what the commit
// list's looks in where they apply (see `SearchKind`); returns whether `key` toggled one, the
// search to be applied again then.
pub fn toggle_search_option(ctx: &AppContext, key: &KeyEvent, kind: SearchKind) -> bool {
    let mut options = ctx.search_options.get();
    match ctx.keybind.event_in(Scope::Global, key) {
        Some(UserEvent::SmartCaseToggle) => options.smart_case = !options.smart_case,
        Some(UserEvent::FuzzyToggle) if kind.has_fuzzy() => options.fuzzy = !options.fuzzy,
        Some(UserEvent::SearchTargetToggle) if kind.has_target() => {
            options.target = options.target.next()
        }
        _ => return false,
    }
    ctx.search_options.set(options);
    true
}

// A key typed into a search or filter: toggling an option applies it again, other keys edit it.
// Returns what to apply.
pub fn type_query(
    ctx: &AppContext,
    query: &mut TreeQuery,
    key: KeyEvent,
    kind: SearchKind,
) -> Option<QueryKind> {
    if toggle_search_option(ctx, &key, kind) {
        query.editing_change()
    } else {
        query.handle_key(key)
    }
}

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
    let mut spans = text(revision.commit.as_str(), 7, theme.list_hash_fg);
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
pub fn query_status(ctx: &AppContext, query: &TreeQuery, kind: SearchKind) -> AppEvent {
    match query.status_input() {
        Some((message, cursor_position)) => AppEvent::UpdateStatusInput {
            message,
            cursor_position,
            metadata: ctx.search_options.get().status_string(kind),
        },
        None => AppEvent::ClearStatusLine,
    }
}

// The status line telling the search for `query`, of `kind`, matched nothing, alike on every page.
pub fn not_found(ctx: &AppContext, query: String, kind: SearchKind) -> AppEvent {
    AppEvent::UpdateSearchResult {
        outcome: SearchOutcome::NotFound { query },
        options: ctx.search_options.get().status_string(kind),
    }
}

// Copies the selected part of a diff or a file.
pub fn copy_selection(text: String, part: DiffPart) -> AppEvent {
    let name = match part {
        DiffPart::Line => "Line",
        DiffPart::Hunk => "Hunk",
    };
    AppEvent::copy(name, text)
}

// The value `copy_path` and `copy_paths` copy: relative to the top of the repository, or absolute
// in full.
fn path_value(repository: &Repository, relative: &str, form: CopyForm) -> String {
    let relative = if relative == ROOT_PATH { "." } else { relative };
    match form {
        CopyForm::Full => {
            let absolute = repository.path().join(relative);
            let absolute = absolute.to_string_lossy();
            absolute.trim_end_matches("/.").to_string()
        }
        CopyForm::Short => relative.to_string(),
    }
}

// Copies the path of a file in the repository: relative to its top, or absolute in full.
pub fn copy_path(repository: &Repository, row: &FileTreeRow, form: CopyForm) -> AppEvent {
    // the root row stands for the whole repository
    let relative = row.entry().map_or(".", Entry::path);
    let (name, value) = match form {
        CopyForm::Full => ("Absolute Path", path_value(repository, relative, form)),
        CopyForm::Short => ("Path", path_value(repository, relative, form)),
    };
    AppEvent::copy(name, value)
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

// Two revisions compared in the details: commits, or refs diffed from the sidebar, which are
// resolved again once reloaded.
#[derive(Debug, Clone)]
pub struct Compare {
    pub base: Rev,
    pub target: Rev,
}

impl Compare {
    pub fn title(&self) -> String {
        format!("Diff {} → {}", self.base.shown(), self.target.shown())
    }
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
