// What the pages share: the key hints of the commit graph and of typing, the status line, copying,
// and what each page keeps across a reload of the repository.

use ratatui::{crossterm::event::KeyEvent, style::Style, text::Span};

use crate::{
    app::AppContext,
    browse::LinkTarget,
    event::{hint, AppEvent, KeyHint, UserEvent},
    git::{CommitHash, FileRevision, Repository},
    keybind::Scope,
    search::{SearchKind, SearchOptions},
    widget::{
        commit_list::format_date,
        commit_list::{CommitListState, SearchRefreshContext},
        commit_tabs::DetailTab,
        file_tree::ROOT_PATH,
        tree_query::{highlighted_text, name_matches, QueryChange, TreeQuery},
    },
};

// The keys of the commit graph, with the commands on the selected or picked commits.
pub fn graph_hints(picked: usize) -> Vec<KeyHint> {
    let mut hints = vec![];
    if picked > 0 {
        hints.push(hint(UserEvent::Cancel, "clear picks"));
    }
    hints.extend([
        hint(UserEvent::ToggleSelect, "pick"),
        hint(UserEvent::Diff, "diff"),
        hint(UserEvent::Checkout, "checkout"),
        hint(UserEvent::Merge, "merge"),
        hint(UserEvent::Rebase, "rebase"),
        hint(UserEvent::CherryPick, "cherry-pick"),
        hint(UserEvent::Squash, "squash"),
        hint(UserEvent::AmendCommit, "amend"),
        hint(UserEvent::DropCommit, "drop"),
        hint(UserEvent::CreateBranch, "branch"),
        hint(UserEvent::CreateTag, "tag"),
        hint(UserEvent::BranchOnly, "branch only"),
        hint(UserEvent::OpenInBrowser, "open in browser"),
        hint(UserEvent::CopyLink, "copy link"),
        hint(UserEvent::Revert, "revert"),
        hint(UserEvent::Reset, "reset"),
        hint(UserEvent::Search, "search"),
        (
            vec![UserEvent::ShortCopy, UserEvent::FullCopy],
            "copy hash".into(),
        ),
    ]);
    hints
}

// The keys while typing a search or filter.
pub fn typing_hints(kind: SearchKind) -> Vec<KeyHint> {
    let mut hints = vec![];
    if kind.has_target() {
        hints.push(hint(UserEvent::SearchTargetToggle, "target"));
    }
    hints.push(hint(UserEvent::SmartCaseToggle, "smart case"));
    if kind.has_fuzzy() {
        hints.push(hint(UserEvent::FuzzyToggle, "fuzzy"));
    }
    hints.extend([
        hint(UserEvent::Confirm, "apply"),
        hint(UserEvent::Cancel, "cancel"),
    ]);
    hints
}

// Copies a commit hash: short, or in full with `full`.
pub fn copy_hash(hash: &CommitHash, full: bool) -> AppEvent {
    let (name, value) = if full {
        ("Commit SHA", hash.as_str())
    } else {
        ("Commit SHA (short)", hash.as_short_hash())
    };
    AppEvent::copy(name, value)
}

// The keys acting on a commit wherever it is shown: copying its hash, opening its page, checking
// it out and selecting it on the graph.
pub fn commit_event(event: UserEvent, commit: &CommitHash) -> Option<AppEvent> {
    match event {
        UserEvent::ShortCopy | UserEvent::FullCopy => {
            Some(copy_hash(commit, event == UserEvent::FullCopy))
        }
        UserEvent::OpenInBrowser | UserEvent::CopyLink => Some(AppEvent::Link {
            target: LinkTarget::Commit(commit.as_str().into()),
            copy: event == UserEvent::CopyLink,
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

// The keys moving in a list of lines, for the key hints of the blame and the timeline.
pub fn moving_hints() -> Vec<KeyHint> {
    use UserEvent::*;
    vec![
        (vec![NavigateDown, NavigateUp], "move".into()),
        (vec![GoToTop, GoToBottom], "top/bottom".into()),
        (vec![HalfPageDown, HalfPageUp], "half page".into()),
        (vec![PageDown, PageUp], "page".into()),
        (vec![ScrollDown, ScrollUp], "scroll".into()),
    ]
}

// The keys of `commit_event`, for the key hints.
pub fn commit_hints() -> [KeyHint; 5] {
    use UserEvent::*;
    [
        hint(Confirm, "show on graph"),
        hint(Checkout, "checkout"),
        hint(OpenInBrowser, "open in browser"),
        hint(CopyLink, "copy link"),
        (vec![ShortCopy, FullCopy], "copy hash".into()),
    ]
}

// o and O on a file at a commit, at the selected lines: its page on the web host, or its link.
pub fn file_link(
    event: UserEvent,
    commit: &CommitHash,
    path: &str,
    lines: Option<(usize, usize)>,
) -> AppEvent {
    let target = LinkTarget::File {
        commit: commit.as_str().into(),
        path: path.into(),
        lines,
    };
    AppEvent::Link {
        target,
        copy: event == UserEvent::CopyLink,
    }
}

// The options toggled while typing a search other than the commit list's, as there: smart case,
// and fuzzy matching where it applies (see `SearchKind`); returns whether `key` toggled one, the
// search to be applied again then.
pub fn toggle_search_option(ctx: &AppContext, key: &KeyEvent, kind: SearchKind) -> bool {
    let mut options = ctx.search_options.get();
    match ctx.keybind.event_in(Scope::Global, key) {
        Some(UserEvent::SmartCaseToggle) => options.smart_case = !options.smart_case,
        Some(UserEvent::FuzzyToggle) if kind.has_fuzzy() => options.fuzzy = !options.fuzzy,
        _ => return false,
    }
    ctx.search_options.set(options);
    true
}

// A key typed into a search or filter other than the commit list's: toggling an option applies it
// again, other keys edit it. Returns what to apply.
pub fn type_query(
    ctx: &AppContext,
    query: &mut TreeQuery,
    key: KeyEvent,
    kind: SearchKind,
) -> QueryChange {
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

// The status line showing what is `typed` in a search of `kind`, with its options, cleared when
// nothing is typed.
pub fn query_status(ctx: &AppContext, typed: Option<(String, u16)>, kind: SearchKind) -> AppEvent {
    status_input(typed, ctx.search_options.get().status_string(kind))
}

// The status line showing what is `typed`, with its cursor, and `metadata` on the right, like
// a search; cleared when nothing is typed.
pub fn status_input(typed: Option<(String, u16)>, metadata: String) -> AppEvent {
    match typed {
        Some((message, cursor_position)) => AppEvent::UpdateStatusInput {
            message,
            cursor_position,
            metadata,
        },
        None => AppEvent::ClearStatusLine,
    }
}

// Copies the selected part of a diff or a file: a line, or else a hunk.
pub fn copy_selection(text: String, line: bool) -> AppEvent {
    AppEvent::copy(if line { "Line" } else { "Hunk" }, text)
}

// Copies the path of a file in the repository: relative to its top, or absolute with `full`.
pub fn copy_path(repository: &Repository, path: &str, full: bool) -> AppEvent {
    // the root row of the local changes stands for the whole repository
    let relative = if path == ROOT_PATH { "." } else { path };
    if full {
        let absolute = repository.path().join(relative);
        let absolute = absolute.to_string_lossy();
        AppEvent::copy("Absolute Path", absolute.trim_end_matches("/."))
    } else {
        AppEvent::copy("Path", relative)
    }
}

// Two revisions compared in the details: commits, or refs diffed from the sidebar, which are
// resolved again once reloaded.
#[derive(Debug, Clone)]
pub struct Compare {
    pub base: String,
    pub target: String,
    pub title: String,
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

#[derive(Debug, Clone)]
pub struct ListRefreshViewContext {
    pub commit_hash: String,
    pub picked_commit_hashes: Vec<CommitHash>,
    pub selected: usize,
    pub height: usize,
    pub scroll_to_top: bool,
    pub search_options: SearchOptions,
    pub search_context: Option<SearchRefreshContext>,
}

impl From<&CommitListState<'_>> for ListRefreshViewContext {
    fn from(list_state: &CommitListState) -> Self {
        let commit_hash = list_state.selected_commit_hash().as_str().into();
        let (selected, offset, height) = list_state.current_list_status();
        // If the selected commit is the top one and there is no offset, it means the list is already scrolled to the top.
        // In this case, we set scroll_to_top to true to indicate that the view should be scrolled to the top after refresh.
        let scroll_to_top = selected == 0 && offset == 0;
        let search_options = list_state.search_options();
        let search_context = list_state.search_refresh_context();
        ListRefreshViewContext {
            commit_hash,
            picked_commit_hashes: list_state.picked_commit_hashes().to_vec(),
            selected,
            height,
            scroll_to_top,
            search_options,
            search_context,
        }
    }
}

pub use crate::widget::local_changes::LocalChangesSnapshot as LocalChangesRefreshViewContext;
