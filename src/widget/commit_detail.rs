use std::rc::Rc;

use chrono::{DateTime, FixedOffset};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Padding, Paragraph, StatefulWidget, Widget},
};
use rustc_hash::FxHashMap;

use crate::{
    app::AppContext,
    git::{Commit, FileChange, Head, Ref, Tracking},
    widget::{
        commit_list::{refs_spans, CommitInfo},
        style::{change_span, divider, render_scrollbar, ScrollState},
    },
};

pub struct CommitDetail<'a> {
    commit: &'a Commit,
    changes: &'a Vec<FileChange>,
    refs: &'a Vec<Ref>,
    head: &'a Head,
    tracking: &'a FxHashMap<String, Tracking>,
    // the details have the focus, the scrollbar in the accent's color
    focused: bool,
    ctx: Rc<AppContext>,
}

impl<'a> CommitDetail<'a> {
    pub fn new(
        commit: &'a Commit,
        changes: &'a Vec<FileChange>,
        refs: &'a Vec<Ref>,
        head: &'a Head,
        tracking: &'a FxHashMap<String, Tracking>,
        focused: bool,
        ctx: Rc<AppContext>,
    ) -> Self {
        Self {
            commit,
            changes,
            refs,
            head,
            tracking,
            focused,
            ctx,
        }
    }
}

impl StatefulWidget for CommitDetail<'_> {
    type State = ScrollState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let [labels_area, value_area] =
            Layout::horizontal([Constraint::Length(12), Constraint::Min(0)]).areas(area);

        let (mut label_lines, mut value_lines) = self.contents(area);

        state.fit(value_lines.len(), area.height as usize, 0);
        // on the right border, like the diffs'
        let border = Rect::new(area.right(), area.y, 1, area.height);
        let theme = &self.ctx.color_theme;
        render_scrollbar(
            buf,
            border,
            value_lines.len(),
            state.offset,
            self.focused,
            theme,
        );

        label_lines = label_lines.into_iter().skip(state.offset).collect();
        value_lines = value_lines.into_iter().skip(state.offset).collect();

        let style = Style::default().fg(self.ctx.color_theme.fg);
        Paragraph::new(label_lines)
            .style(style)
            .block(Block::default().padding(Padding::left(1)))
            .render(labels_area, buf);
        Paragraph::new(value_lines)
            .style(style)
            .block(Block::default().padding(Padding::horizontal(1)))
            .render(value_area, buf);
    }
}

impl CommitDetail<'_> {
    fn contents(&self, area: Rect) -> (Vec<Line<'_>>, Vec<Line<'_>>) {
        let mut label_lines: Vec<Line> = Vec::new();
        let mut value_lines: Vec<Line> = Vec::new();

        label_lines.push(Line::from("   Author: ").fg(self.ctx.color_theme.detail_label_fg));
        label_lines.push(self.empty_line());
        value_lines.extend(self.author_lines());

        if is_author_committer_different(self.commit) {
            label_lines.push(Line::from("Committer: ").fg(self.ctx.color_theme.detail_label_fg));
            label_lines.push(self.empty_line());
            value_lines.extend(self.committer_lines());
        }

        label_lines.push(Line::from("      SHA: ").fg(self.ctx.color_theme.detail_label_fg));
        value_lines.push(self.sha_line());

        if has_parent(self.commit) {
            label_lines.push(Line::from("  Parents: ").fg(self.ctx.color_theme.detail_label_fg));
            value_lines.push(self.parents_line());
        }

        if !self.refs.is_empty() || self.detached_here() {
            label_lines.push(Line::from("     Refs: ").fg(self.ctx.color_theme.detail_label_fg));
            value_lines.push(self.refs_line());
        }

        value_lines.push(self.divider_line(area.width as usize));
        value_lines.extend(self.commit_message_lines());

        value_lines.push(self.divider_line(area.width as usize));
        value_lines.extend(self.changes_lines());

        (label_lines, value_lines)
    }

    fn author_lines(&self) -> Vec<Line<'_>> {
        self.author_committer_lines(
            &self.commit.author_name,
            &self.commit.author_email,
            &self.commit.author_date,
        )
    }

    fn committer_lines(&self) -> Vec<Line<'_>> {
        self.author_committer_lines(
            &self.commit.committer_name,
            &self.commit.committer_email,
            &self.commit.committer_date,
        )
    }

    fn author_committer_lines<'a>(
        &'a self,
        name: &'a str,
        email: &'a str,
        date: &'a DateTime<FixedOffset>,
    ) -> Vec<Line<'a>> {
        let date_str = if self.ctx.ui_config.detail.date_local {
            let local = date.with_timezone(&chrono::Local);
            local
                .format(&self.ctx.ui_config.detail.date_format)
                .to_string()
        } else {
            date.format(&self.ctx.ui_config.detail.date_format)
                .to_string()
        };
        vec![
            Line::from(vec![
                name.fg(self.ctx.color_theme.detail_name_fg),
                " <".into(),
                email.fg(self.ctx.color_theme.detail_email_fg),
                "> ".into(),
            ]),
            Line::from(date_str.fg(self.ctx.color_theme.detail_date_fg)),
        ]
    }

    fn sha_line(&self) -> Line<'_> {
        Line::from(
            self.commit
                .commit_hash
                .as_str()
                .fg(self.ctx.color_theme.detail_hash_fg),
        )
    }

    fn parents_line(&self) -> Line<'_> {
        let mut spans = Vec::new();
        let parents = &self.commit.parent_commit_hashes;
        for (i, hash) in parents
            .iter()
            .map(|hash| hash.as_short_hash().fg(self.ctx.color_theme.detail_hash_fg))
            .enumerate()
        {
            spans.push(hash);
            if i < parents.len() - 1 {
                spans.push(Span::raw(" "));
            }
        }
        Line::from(spans)
    }

    // The labels of the commit list.
    fn refs_line(&self) -> Line<'_> {
        let info = CommitInfo::new(self.commit, self.refs.iter().collect(), self.tracking, true);
        let options = self.ctx.search_options.get();
        Line::from(refs_spans(
            &info,
            None,
            options,
            self.head,
            &self.ctx.color_theme,
        ))
    }

    fn detached_here(&self) -> bool {
        matches!(self.head, Head::Detached { target } if *target == self.commit.commit_hash)
    }

    fn commit_message_lines(&self) -> Vec<Line<'_>> {
        let subject_line = Line::from(self.commit.subject.as_str().bold());

        let mut lines = vec![subject_line];

        if self.commit.body.is_empty() {
            return lines;
        }

        let body_lines = self.commit.body.lines().map(Line::raw);

        lines.push(self.empty_line());
        lines.extend(body_lines);

        lines
    }

    fn changes_lines(&self) -> Vec<Line<'_>> {
        self.changes
            .iter()
            .map(|c| {
                let path = match c {
                    FileChange::Add { path }
                    | FileChange::Modify { path }
                    | FileChange::Delete { path } => path.clone(),
                    FileChange::Move { from, to } => format!("{from} -> {to}"),
                };
                let symbol = change_span(c.symbol(), &self.ctx.color_theme);
                Line::from(vec![symbol, " ".into(), path.into()])
            })
            .collect()
    }

    fn empty_line(&self) -> Line<'_> {
        Line::raw("")
    }

    fn divider_line(&self, width: usize) -> Line<'_> {
        Line::from(divider(width, &self.ctx.color_theme))
    }
}

fn is_author_committer_different(commit: &Commit) -> bool {
    commit.author_name != commit.committer_name
        || commit.author_email != commit.committer_email
        || commit.author_date != commit.committer_date
}

fn has_parent(commit: &Commit) -> bool {
    !commit.parent_commit_hashes.is_empty()
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;
    use crate::test_support::ui::{column_of, find_row, row_text, test_ctx};

    #[test]
    fn test_long_details_scrollbar_on_the_border() {
        let body: Vec<String> = (1..=60).map(|i| format!("line {i}")).collect();
        let commit = Commit {
            body: body.join("\n"),
            ..Default::default()
        };
        let tracking = FxHashMap::default();
        // the border right of the details
        let area = Rect::new(0, 0, 60, 10);
        let mut buf = Buffer::empty(Rect::new(0, 0, 61, 10));
        CommitDetail::new(
            &commit,
            &vec![],
            &vec![],
            &Head::None,
            &tracking,
            true,
            test_ctx(),
        )
        .render(area, &mut buf, &mut ScrollState::default());
        let border: String = (0..10).map(|y| buf[(60, y)].symbol()).collect();
        assert!(border.contains('█'), "{border:?}");
    }

    #[test]
    fn test_refs_look_like_the_commit_list() {
        let commit = Commit::default();
        let target = commit.commit_hash.clone();
        let refs = vec![
            Ref::Branch {
                name: "main".into(),
                target: target.clone(),
            },
            Ref::RemoteBranch {
                name: "fork/main".into(),
                target: target.clone(),
            },
            Ref::Tag {
                name: "v1".into(),
                target: target.clone(),
            },
            Ref::Stash {
                name: "stash@{0}".into(),
                message: "wip".into(),
                target,
            },
        ];
        let tracking = FxHashMap::default();
        let ctx = test_ctx();
        let area = Rect::new(0, 0, 100, 20);
        let mut detail = Buffer::empty(area);
        CommitDetail::new(
            &commit,
            &vec![],
            &refs,
            &Head::None,
            &tracking,
            true,
            ctx.clone(),
        )
        .render(area, &mut detail, &mut ScrollState::default());

        // the labels as the commit list draws them, cell for cell
        let info = CommitInfo::new(&commit, refs.iter().collect(), &tracking, true);
        let spans = refs_spans(
            &info,
            None,
            ctx.search_options.get(),
            &Head::None,
            &ctx.color_theme,
        );
        let mut list = Buffer::empty(Rect::new(0, 0, 100, 1));
        Line::from(spans).render(list.area, &mut list);
        let width = list
            .content()
            .iter()
            .rposition(|c| c.symbol() != " ")
            .unwrap() as u16
            + 1;
        let y = find_row(&detail, "v1");
        let x = column_of(&detail, y, &row_text(&list, 0)[..6]);
        for i in 0..width {
            let (shown, expected) = (&detail[(x + i, y)], &list[(i, 0)]);
            // the text color of a blank between labels shows nowhere
            if expected.symbol() == " " && expected.bg == Color::Reset {
                assert_eq!(shown.bg, Color::Reset, "column {i}");
            } else {
                assert_eq!(shown, expected, "column {i}");
            }
        }
    }
}
