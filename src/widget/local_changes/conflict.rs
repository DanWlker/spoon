// The conflict panel of the local changes page, shown beside the files in place of the diff of a
// conflicted file: its sides, and the side or the tool to resolve it with.

use super::*;

// The choices of the conflict panel: a side to keep, or a tool to resolve the conflicts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConflictItem {
    Existing,
    Incoming,
    MergeTool,
    Editor,
}

// Shown beside the files instead of the diff of a conflicted file: its two sides, and the choice
// selected while the panel has the focus.
#[derive(Debug)]
pub(super) struct ConflictPanel {
    pub(super) view: ConflictView,
    pub(super) item: ConflictItem,
}

impl LocalChangesState<'_> {
    pub fn is_conflict_focused(&self) -> bool {
        self.focus() == Focus::Side && self.conflict().is_some()
    }

    // Moves between the sides (side by side) and the tools below them.
    pub fn move_conflict_item(&mut self, direction: Direction) {
        use ConflictItem::*;
        let Some(ConflictPanel { item, view }) = self.conflict_mut() else {
            return;
        };
        *item = match (*item, direction) {
            (Existing, Direction::Right) => Incoming,
            (Incoming, Direction::Left) => Existing,
            (Existing | Incoming, Direction::Down) => MergeTool,
            // no file to open for a symlink or a submodule, see `ConflictView::editable`
            (MergeTool, Direction::Down) if view.editable => Editor,
            (Editor, Direction::Up) => MergeTool,
            (MergeTool, Direction::Up) => Existing,
            (item, _) => item,
        };
    }

    pub(super) fn activate_conflict_item(&mut self, item: ConflictItem) -> Result<(), String> {
        let Some(path) = self.selected_path() else {
            return Ok(());
        };
        let side = match item {
            ConflictItem::Existing => ConflictSide::Ours,
            ConflictItem::Incoming => ConflictSide::Theirs,
            ConflictItem::MergeTool => {
                self.request = Some(Request::External(ExternalRequest::MergeTool(path)));
                return Ok(());
            }
            ConflictItem::Editor => {
                let path = crate::git::work_path(self.repository.path(), &path);
                self.request = Some(Request::External(ExternalRequest::Editor(vec![path])));
                return Ok(());
            }
        };
        let result = self.repository.resolve_conflict(&path, side);
        self.chosen = Focus::Files;
        self.reload();
        result
    }

    // The two sides of the selected file, when it is conflicted.
    pub(super) fn load_conflict(&self) -> Option<ConflictPanel> {
        self.selected_row()
            .and_then(FileTreeRow::file_index)
            .and_then(|i| self.files().get(i))
            .filter(|f| f.status == LocalChangeStatus::Conflicted)
            .map(|f| ConflictPanel {
                view: ConflictView::load(self.repository, &f.path),
                item: ConflictItem::Existing,
            })
    }

    // The conflict shown beside the files, see `Beside`.
    pub(super) fn conflict(&self) -> Option<&ConflictPanel> {
        match &self.beside {
            Beside::Conflict(conflict) => Some(conflict),
            _ => None,
        }
    }

    pub(super) fn conflict_mut(&mut self) -> Option<&mut ConflictPanel> {
        match &mut self.beside {
            Beside::Conflict(conflict) => Some(conflict),
            _ => None,
        }
    }
}

impl LocalChangesPanel<'_> {
    pub(super) fn render_conflict(
        &self,
        state: &mut LocalChangesState,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let Beside::Conflict(conflict) = &state.beside else {
            return;
        };
        let theme = &self.ctx.color_theme;
        let path = state.selected_path().unwrap_or_default();
        let inner = render_panel(
            buf,
            area,
            Line::from(vec![
                Span::raw(format!(" {path} ")).fg(theme.fg),
                badge(" conflicted ", theme.status_error_fg, theme),
            ]),
            self.focused && state.is_conflict_focused(),
            theme,
        );

        // the sides to pick from, side by side over half the height (the common ancestor between
        // them when the markers have it), then what Enter does, then the tools with a line between
        // each, all centered vertically
        let [sides_area, hint_area, tools_area] = Layout::vertical([
            Constraint::Percentage(50),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .flex(Flex::Center)
        .areas(inner);
        let selected_item = (self.focused && state.is_conflict_focused()).then_some(conflict.item);
        let conflict = &conflict.view;
        let focused = |item| selected_item == Some(item);
        let (ours_name, theirs_name) = side_names(state.repository.operation_in_progress());

        let mut sides = vec![(
            Some(ConflictItem::Existing),
            ours_name,
            &conflict.ours_label,
            &conflict.ours,
        )];
        let empty = String::new();
        if let Some(base) = &conflict.base {
            sides.push((None, "Base", &empty, base));
        }
        sides.push((
            Some(ConflictItem::Incoming),
            theirs_name,
            &conflict.theirs_label,
            &conflict.theirs,
        ));
        let side_areas =
            Layout::horizontal(vec![Constraint::Fill(1); sides.len()]).split(sides_area);
        let number_width = conflict.last_number().map_or(0, |n| n.to_string().len());
        let longest = conflict.ours.len().max(conflict.theirs.len());
        let scrolloff = self.ctx.ui_config.common.scrolloff as usize;
        let mut height = 0;
        for ((item, name, label, rows), side_area) in sides.into_iter().zip(side_areas.iter()) {
            let is_focused = item.is_some_and(focused);
            // the name git put on the markers, like "HEAD"
            let note = (!label.is_empty()).then(|| label.clone());
            let title = panel_title(name, note, is_focused, theme);
            let content_area = render_panel(buf, *side_area, title, is_focused, theme);
            height = content_area.height as usize;
            let range = state.pane.scroll_state().fit(longest, height, scrolloff);
            render_panel_scrollbar(buf, *side_area, longest, range.start, is_focused, theme);
            let lines: Vec<Line> = rows
                .iter()
                .skip(range.start)
                .take(height)
                .map(|row| {
                    let fg = match row.kind {
                        RowKind::Header | RowKind::Context => theme.muted_fg,
                        RowKind::Kept => theme.fg,
                        RowKind::Changed => theme.detail_file_change_add_fg,
                    };
                    let number = match row.number {
                        _ if number_width == 0 => String::new(),
                        Some(n) => format!("{n:>number_width$} "),
                        None => " ".repeat(number_width + 1),
                    };
                    Line::from(vec![
                        Span::raw(number).fg(theme.muted_fg),
                        Span::raw(row.text.clone()).fg(fg),
                    ])
                })
                .collect();
            Paragraph::new(lines).render(content_area, buf);
        }
        state.pane.scroll_state().height = height;

        // what the select key, space, does on the item selected, named as it is bound
        let select = self
            .ctx
            .keybind
            .short_keys(crate::keybind::Scope::LocalChanges, &[UserEvent::Select]);
        let mut chars = select.chars();
        let select: String = chars
            .next()
            .map(|c| c.to_ascii_uppercase())
            .into_iter()
            .chain(chars)
            .collect();
        let hint = match selected_item {
            Some(ConflictItem::Existing) => {
                format!("{select} keeps {ours_name} for the whole file, and stages it")
            }
            Some(ConflictItem::Incoming) => {
                format!("{select} keeps {theirs_name} for the whole file, and stages it")
            }
            Some(ConflictItem::MergeTool) => {
                format!("{select} resolves the conflicts in the merge tool")
            }
            Some(ConflictItem::Editor) => {
                format!("{select} opens the file to edit the markers, staged once none is left")
            }
            None => "Enter to keep a side for the whole file, or to resolve it in a tool".into(),
        };
        Paragraph::new(Line::raw(hint).fg(theme.muted_fg))
            .centered()
            .render(hint_area, buf);

        let tool = match &self.ctx.core_config.git.merge_tool {
            Some(tool) => format!("  Edit in merge tool ({tool})  "),
            None => "  Edit in merge tool  ".to_string(),
        };
        let editor = conflict
            .editable
            .then(|| (ConflictItem::Editor, "  Open in editor  ".into()));
        let tools = std::iter::once((ConflictItem::MergeTool, tool)).chain(editor);
        for (i, (item, text)) in tools.enumerate() {
            let width = (text.chars().count() as u16).min(tools_area.width);
            // centered under the sides
            let x = tools_area.x + (tools_area.width - width) / 2;
            let tool_area = Rect::new(x, tools_area.y + 2 * i as u16, width, 1);
            if tool_area.y >= tools_area.bottom() {
                break;
            }
            // buttons, the focused one filled
            let style = if focused(item) {
                Style::default()
                    .bg(theme.accent_fg)
                    .fg(theme.accent_text_fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg).bg(theme.divider_fg)
            };
            Paragraph::new(Line::styled(text, style)).render(tool_area, buf);
        }
    }
}
