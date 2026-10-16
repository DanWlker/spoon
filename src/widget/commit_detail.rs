use std::rc::Rc;

use chrono::{DateTime, FixedOffset};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Padding, Paragraph, StatefulWidget, Widget},
};

use crate::{
    app::AppContext,
    git::{Commit, FileChange, Head, Ref},
    widget::{
        commit_list::{ref_color, ref_label_style},
        style::{change_span, divider, ScrollState},
    },
};

pub struct CommitDetail<'a> {
    commit: &'a Commit,
    changes: &'a Vec<FileChange>,
    refs: &'a Vec<Ref>,
    head: &'a Head,
    ctx: Rc<AppContext>,
}

impl<'a> CommitDetail<'a> {
    pub fn new(
        commit: &'a Commit,
        changes: &'a Vec<FileChange>,
        refs: &'a Vec<Ref>,
        head: &'a Head,
        ctx: Rc<AppContext>,
    ) -> Self {
        Self {
            commit,
            changes,
            refs,
            head,
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

        state.fit(value_lines.len(), area.height as usize);

        label_lines = label_lines.into_iter().skip(state.offset).collect();
        value_lines = value_lines.into_iter().skip(state.offset).collect();

        self.render_labels_paragraph(label_lines, labels_area, buf);
        self.render_value_paragraph(value_lines, value_area, buf);
    }
}

impl CommitDetail<'_> {
    fn render_labels_paragraph(&self, lines: Vec<Line>, area: Rect, buf: &mut Buffer) {
        let paragraph = Paragraph::new(lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .block(Block::default().padding(Padding::left(1)));
        paragraph.render(area, buf);
    }

    fn render_value_paragraph(&self, lines: Vec<Line>, area: Rect, buf: &mut Buffer) {
        let paragraph = Paragraph::new(lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .block(Block::default().padding(Padding::new(1, 1, 0, 0)));
        paragraph.render(area, buf);
    }

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

        if has_refs(self.refs) || self.detached_here() {
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

    // Labels like those of the commit list.
    fn refs_line(&self) -> Line<'_> {
        let theme = &self.ctx.color_theme;
        let head_branch = match self.head {
            Head::Branch { name } => Some(name.as_str()),
            _ => None,
        };
        let mut spans = Vec::new();
        if self.detached_here() {
            spans.push(Span::styled(
                " HEAD ",
                ref_label_style(theme.list_ref_head_fg),
            ));
            spans.push(Span::raw(" "));
        }
        for r in self.refs.iter().filter(|r| !matches!(r, Ref::Stash { .. })) {
            let style = ref_label_style(ref_color(r, head_branch, theme));
            spans.push(Span::styled(format!(" {} ", r.name()), style));
            spans.push(Span::raw(" "));
        }
        Line::from(spans)
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

fn has_refs(refs: &[Ref]) -> bool {
    refs.iter().any(|r| {
        matches!(
            r,
            Ref::Branch { .. } | Ref::RemoteBranch { .. } | Ref::Tag { .. }
        )
    })
}
