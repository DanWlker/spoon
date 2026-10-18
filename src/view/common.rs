// What the pages share: the key hints of the commit graph and of typing, the status line, copying,
// and what each page keeps across a reload of the repository.

use ratatui::{crossterm::event::KeyEvent, style::Style, text::Span};

use crate::{
    app::AppContext,
    browse::{LinkAction, LinkTarget},
    event::{AppEvent, UserEvent},
    git::{CommitHash, FileRevision, Repository, Rev},
    keybind::Scope,
    keys::{Availability, KeyList, Keys},
    search::{SearchKind, SearchOptions},
    widget::{
        commit_list::format_date,
        commit_list::{CommitListState, SearchRefreshContext},
        commit_tabs::DetailTab,
        file_tree::ROOT_PATH,
        tree_query::{highlighted_text, name_matches, QueryChange, TreeQuery},
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
    lines: Option<(usize, usize)>,
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
