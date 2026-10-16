use std::rc::Rc;

use ansi_to_tui::IntoText as _;
use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    text::Line,
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent},
    git::{Commit, CommitHash, Ref, Repository},
    layout::{fit_size, Panel},
    view::{ListRefreshViewContext, RefreshViewContext, UserCommandRefreshViewContext},
    widget::{
        commit_list::{commit_list_inner_height, render_commit_list_panel, CommitListState},
        commit_tabs::DetailTab,
        commit_user_command::{CommitUserCommand, CommitUserCommandState},
        style::panel_block,
    },
};

type ExecCommandFn =
    fn(&Commit, &[Ref], &[CommitHash], usize, Rect, &AppContext) -> Result<String, String>;

#[derive(Debug)]
pub struct UserCommandView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    commit_user_command_state: CommitUserCommandState,

    user_command_number: usize,
    user_command_output_lines: Vec<Line<'a>>,
    // the selection moved to another commit, the command runs for it once used, see `run_selected`
    stale: bool,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> UserCommandView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        command_output: String,
        user_command_number: usize,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> UserCommandView<'a> {
        let user_command_output_lines =
            build_user_command_output_lines(command_output, ctx.clone()).unwrap_or_else(|err| {
                tx.send(AppEvent::NotifyError(err));
                vec![]
            });

        UserCommandView {
            commit_list_state: Some(commit_list_state),
            commit_user_command_state: CommitUserCommandState::default(),
            user_command_number,
            user_command_output_lines,
            stale: false,
            ctx,
            tx,
        }
    }

    pub fn handle_event(&mut self, event: UserEvent, _: KeyEvent) {
        match event {
            UserEvent::NavigateDown => {
                self.commit_user_command_state.scroll_down();
            }
            UserEvent::NavigateUp => {
                self.commit_user_command_state.scroll_up();
            }
            UserEvent::PageDown => {
                self.commit_user_command_state.scroll_page_down();
            }
            UserEvent::PageUp => {
                self.commit_user_command_state.scroll_page_up();
            }
            UserEvent::HalfPageDown => {
                self.commit_user_command_state.scroll_half_page_down();
            }
            UserEvent::HalfPageUp => {
                self.commit_user_command_state.scroll_half_page_up();
            }
            UserEvent::GoToTop => {
                self.commit_user_command_state.select_first();
            }
            UserEvent::GoToBottom => {
                self.commit_user_command_state.select_last();
            }
            UserEvent::SelectDown => {
                self.tx.send(AppEvent::SelectOlderCommit);
            }
            UserEvent::SelectUp => {
                self.tx.send(AppEvent::SelectNewerCommit);
            }
            UserEvent::HelpToggle => {
                self.tx.send(AppEvent::OpenHelp);
            }
            UserEvent::UserCommand(n) => {
                if n == self.user_command_number {
                    self.tx.send(AppEvent::CloseUserCommand);
                } else {
                    // switch to another user command
                    self.tx.send(AppEvent::OpenUserCommand(n));
                }
            }
            UserEvent::Confirm => {
                self.tx.send(AppEvent::OpenDetail(DetailTab::Info));
            }
            UserEvent::Cancel | UserEvent::Close => {
                self.tx.send(AppEvent::CloseUserCommand);
            }
            UserEvent::Refresh => {
                self.refresh();
            }
            UserEvent::Quit => {
                self.tx.send(AppEvent::Quit);
            }
            _ => {}
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let [list_area, user_command_area] = self.split_areas(area);

        let ctx = self.ctx.clone();
        render_commit_list_panel(f, list_area, self.as_mut_list_state(), ctx, false);

        let block = panel_block(true, &self.ctx.color_theme);
        let inner = block.inner(user_command_area);
        f.render_widget(block, user_command_area);
        self.ctx
            .layout
            .borrow_mut()
            .place(Panel::UserCommand, user_command_area, false, false);
        let commit_user_command =
            CommitUserCommand::new(&self.user_command_output_lines, self.ctx.clone());
        f.render_stateful_widget(
            commit_user_command,
            inner,
            &mut self.commit_user_command_state,
        );
    }

    pub fn update_layout(&mut self, area: Rect) {
        let [list_area, _] = self.split_areas(area);
        self.as_mut_list_state()
            .update_height(commit_list_inner_height(list_area.height));
    }

    pub fn prepare_graph_uploads(&mut self) {
        self.as_mut_list_state().ensure_visible_graph_uploaded();
    }
}

impl<'a> UserCommandView<'a> {
    pub fn take_list_state(&mut self) -> CommitListState<'a> {
        self.commit_list_state.take().unwrap()
    }

    fn as_mut_list_state(&mut self) -> &mut CommitListState<'a> {
        self.commit_list_state.as_mut().unwrap()
    }

    pub fn as_list_state(&self) -> &CommitListState<'a> {
        self.commit_list_state.as_ref().unwrap()
    }

    pub fn drain_pending_graph_uploads(&mut self) -> Vec<String> {
        self.as_mut_list_state().drain_pending_graph_uploads()
    }

    pub fn graph_image_ids_sorted(&self) -> Vec<u32> {
        self.as_list_state().graph_image_ids_sorted()
    }

    pub fn image_clear_area(&self, area: Rect) -> Rect {
        let [_, user_command_area] = self.split_areas(area);
        user_command_area
    }

    fn split_areas(&self, area: Rect) -> [Rect; 2] {
        let user_command_height = user_command_height(&self.ctx, area.height);
        Layout::vertical([Constraint::Min(0), Constraint::Length(user_command_height)]).areas(area)
    }

    // Moves the selection; the command runs for the commit once the key is let go, see `run_selected`.
    pub fn select_older_commit(&mut self) {
        self.as_mut_list_state().select_next();
        self.stale = true;
    }

    pub fn select_newer_commit(&mut self) {
        self.as_mut_list_state().select_prev();
        self.stale = true;
    }

    // Runs the command for the selected commit, when the selection moved since it last ran.
    pub fn run_selected(
        &mut self,
        repository: &Repository,
        view_area: Rect,
        exec_command: ExecCommandFn,
    ) {
        if !std::mem::take(&mut self.stale) {
            return;
        }
        let commit_list_state = self.as_list_state();
        let selected = commit_list_state.selected_commit_hash().clone();
        let picked_commit_hashes = commit_list_state.picked_commit_hashes().to_vec();
        let (commit, _) = repository.commit_detail(&selected);
        let refs: Vec<Ref> = repository.refs(&selected).into_iter().cloned().collect();
        self.user_command_output_lines = exec_command(
            &commit,
            &refs,
            &picked_commit_hashes,
            self.user_command_number,
            view_area,
            &self.ctx,
        )
        .and_then(|output| build_user_command_output_lines(output, self.ctx.clone()))
        .unwrap_or_else(|err| {
            self.tx.send(AppEvent::NotifyError(err));
            vec![]
        });

        self.commit_user_command_state.select_first();
    }

    pub fn refresh(&self) {
        self.tx.send(AppEvent::Refresh(self.refresh_context()));
    }

    // What to show again once the repository is reloaded.
    pub fn refresh_context(&self) -> RefreshViewContext {
        let list_context = ListRefreshViewContext::from(self.as_list_state());
        let user_command_context = UserCommandRefreshViewContext {
            n: self.user_command_number,
        };
        RefreshViewContext::UserCommand {
            list_context,
            user_command_context,
        }
    }
}

// The height of the output of a user command under the commit list, in a page `height` high.
pub fn user_command_height(ctx: &AppContext, height: u16) -> u16 {
    let default = ctx.ui_config.user_command.height;
    let user_command_height = ctx.layout.borrow().size(Panel::UserCommand, default);
    // the commit list keeps a few rows
    fit_size(user_command_height, height, 3, 4)
}

fn build_user_command_output_lines<'a>(
    command_output: String,
    ctx: Rc<AppContext>,
) -> Result<Vec<Line<'a>>, String> {
    let tab_spaces = " ".repeat(ctx.core_config.user_command.tab_width as usize);
    command_output
        .replace('\t', &tab_spaces) // tab is not rendered correctly, so replace it
        .into_text()
        .map(|t| t.into_iter().collect())
        .map_err(|e| e.to_string())
}
