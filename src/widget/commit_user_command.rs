use std::rc::Rc;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{Block, Padding, Paragraph, StatefulWidget, Widget},
};

use crate::{app::AppContext, widget::style::ScrollState};

pub struct CommitUserCommand<'a> {
    lines: &'a Vec<Line<'a>>,
    ctx: Rc<AppContext>,
}

impl<'a> CommitUserCommand<'a> {
    pub fn new(lines: &'a Vec<Line<'a>>, ctx: Rc<AppContext>) -> Self {
        Self { lines, ctx }
    }
}

impl StatefulWidget for CommitUserCommand<'_> {
    type State = ScrollState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        state.fit(self.lines.len(), area.height as usize);

        self.render_user_command_lines(area, buf, state);
    }
}

impl CommitUserCommand<'_> {
    fn render_user_command_lines(&self, area: Rect, buf: &mut Buffer, state: &mut ScrollState) {
        let lines = self
            .lines
            .iter()
            .skip(state.offset)
            .take(area.height as usize)
            .cloned()
            .collect::<Vec<_>>();
        let paragraph = Paragraph::new(lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .block(Block::default().padding(Padding::horizontal(1)));
        paragraph.render(area, buf);
    }
}
