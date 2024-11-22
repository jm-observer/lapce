use floem::views::editor::phantom_text::{PhantomText, PhantomTextKind};
use floem::{
    context::PaintCx,
    peniko::kurbo::{Point, Rect, Size},
    reactive::{Memo, SignalGet, SignalWith},
    text::{Attrs, AttrsList, FamilyOwned, TextLayout},
    Renderer, View, ViewId,
};
use im::HashMap;
use lsp_types::Position;
use serde::{Deserialize, Serialize};

use lapce_core::buffer::Buffer;
use lapce_core::{buffer::rope_text::RopeText, mode::Mode};

use crate::config::{color::LapceColor, LapceConfig};
use crate::editor::screen_lines::ScreenLines;

use super::{view::changes_colors_screen, EditorData};

pub struct EditorGutterView {
    id: ViewId,
    editor: EditorData,
    width: f64,
    gutter_padding_right: Memo<f32>,
}

pub fn editor_gutter_view(
    editor: EditorData,
    gutter_padding_right: Memo<f32>,
) -> EditorGutterView {
    let id = ViewId::new();

    EditorGutterView {
        id,
        editor,
        width: 0.0,
        gutter_padding_right,
    }
}

impl EditorGutterView {
    fn paint_head_changes(
        &self,
        cx: &mut PaintCx,
        e_data: &EditorData,
        viewport: Rect,
        is_normal: bool,
        config: &LapceConfig,
    ) {
        if !is_normal {
            return;
        }

        let changes = e_data.doc().head_changes().get_untracked();
        let line_height = config.editor.line_height() as f64;
        let gutter_padding_right = self.gutter_padding_right.get_untracked() as f64;

        let changes = changes_colors_screen(config, &e_data.editor, changes);
        for (y, height, removed, color) in changes {
            let height = if removed {
                10.0
            } else {
                height as f64 * line_height
            };
            let mut y = y - viewport.y0;
            if removed {
                y -= 5.0;
            }
            cx.fill(
                &Size::new(3.0, height).to_rect().with_origin(Point::new(
                    self.width + 5.0 - gutter_padding_right,
                    y,
                )),
                color,
                0.0,
            )
        }
    }

    fn paint_sticky_headers(
        &self,
        cx: &mut PaintCx,
        is_normal: bool,
        config: &LapceConfig,
    ) {
        if !is_normal {
            return;
        }

        if !config.editor.sticky_header {
            return;
        }
        let sticky_header_height = self.editor.sticky_header_height.get_untracked();
        if sticky_header_height == 0.0 {
            return;
        }

        let sticky_area_rect =
            Size::new(self.width + 25.0 + 30.0, sticky_header_height)
                .to_rect()
                .with_origin(Point::new(-25.0, 0.0))
                .inflate(25.0, 0.0);
        cx.fill(
            &sticky_area_rect,
            config.color(LapceColor::LAPCE_DROPDOWN_SHADOW),
            3.0,
        );
        cx.fill(
            &sticky_area_rect,
            config.color(LapceColor::EDITOR_STICKY_HEADER_BACKGROUND),
            0.0,
        );
    }
}

impl View for EditorGutterView {
    fn id(&self) -> ViewId {
        self.id
    }

    fn compute_layout(
        &mut self,
        _cx: &mut floem::context::ComputeLayoutCx,
    ) -> Option<floem::peniko::kurbo::Rect> {
        if let Some(width) = self.id.get_layout().map(|l| l.size.width as f64) {
            self.width = width;
        }
        None
    }

    fn paint(&mut self, cx: &mut floem::context::PaintCx) {
        let viewport = self.editor.viewport();
        let cursor = self.editor.cursor();
        let screen_lines = self.editor.screen_lines();
        let config = self.editor.common.config;

        let kind_is_normal =
            self.editor.kind().with_untracked(|kind| kind.is_normal());
        let (offset, mode) = cursor.with_untracked(|c| (c.offset(), c.get_mode()));
        let config = config.get_untracked();
        let line_height = config.editor.line_height() as f64;
        let last_line = self.editor.editor.last_line();
        // let current_line = self
        //     .editor
        //     .doc()
        //     .buffer
        //     .with_untracked(|buffer| buffer.line_of_offset(offset));

        let (current_visual_line, _line_offset, _) = self
            .editor
            .editor
            .lines()
            .with_untracked(|x| x.visual_line_of_offset(offset));

        let family: Vec<FamilyOwned> =
            FamilyOwned::parse_list(&config.editor.font_family).collect();
        let attrs = Attrs::new()
            .family(&family)
            .color(config.color(LapceColor::EDITOR_DIM))
            .font_size(config.editor.font_size() as f32);
        let attrs_list = AttrsList::new(attrs);
        let current_line_attrs_list =
            AttrsList::new(attrs.color(config.color(LapceColor::EDITOR_FOREGROUND)));
        let show_relative = config.core.modal
            && config.editor.modal_mode_relative_line_numbers
            && mode != Mode::Insert
            && kind_is_normal;

        let current_number = current_visual_line.line_number(false, None);
        screen_lines.with_untracked(|screen_lines| {
            for visual_line_info in screen_lines.visual_lines.iter() {
                let line_number = visual_line_info
                    .visual_line
                    .line_number(show_relative, current_number);
                let text_layout = if current_number == line_number {
                    TextLayout::new(
                        &line_number.map(|x| x.to_string()).unwrap_or_default(),
                        current_line_attrs_list.clone(),
                    )
                } else {
                    TextLayout::new(
                        &line_number.map(|x| x.to_string()).unwrap_or_default(),
                        attrs_list.clone(),
                    )
                };
                let y = visual_line_info.y;
                let size = text_layout.size();
                let height = size.height;

                cx.draw_text(
                    &text_layout,
                    Point::new(
                        (self.width
                            - size.width
                            - self.gutter_padding_right.get_untracked() as f64)
                            .max(0.0),
                        y + (line_height - height) / 2.0 - viewport.y0,
                    ),
                );
            }
        });

        self.paint_head_changes(cx, &self.editor, viewport, kind_is_normal, &config);
        self.paint_sticky_headers(cx, kind_is_normal, &config);
    }

    fn debug_name(&self) -> std::borrow::Cow<'static, str> {
        "Editor Gutter".into()
    }
}

#[derive(Default, Clone)]
pub struct FoldingRanges(pub Vec<FoldingRange>);

#[derive(Default, Clone)]
pub struct FoldedRanges(pub Vec<FoldedRange>);

impl FoldingRanges {
    pub fn get_folded_range(&self) -> FoldedRanges {
        let mut range = Vec::new();
        let mut limit_line = 0;
        for item in &self.0 {
            if item.start.line < limit_line && limit_line > 0 {
                continue;
            }
            if item.status.is_folded() {
                range.push(crate::editor::gutter::FoldedRange {
                    start: item.start,
                    end: item.end,
                    collapsed_text: item.collapsed_text.clone(),
                });
                limit_line = item.end.line;
            }
        }

        FoldedRanges(range)
    }

    pub fn get_folded_range_by_line(&self, line: u32) -> FoldedRanges {
        let mut range = Vec::new();
        let mut limit_line = 0;
        for item in &self.0 {
            if item.start.line < limit_line && limit_line > 0 {
                continue;
            }
            if item.status.is_folded()
                && item.start.line <= line
                && item.end.line >= line
            {
                range.push(crate::editor::gutter::FoldedRange {
                    start: item.start,
                    end: item.end,
                    collapsed_text: item.collapsed_text.clone(),
                });
                limit_line = item.end.line;
            }
        }

        FoldedRanges(range)
    }
    pub fn to_display_items(&self, lines: ScreenLines) -> Vec<FoldingDisplayItem> {
        let mut folded = HashMap::new();
        let mut unfold_start: HashMap<u32, FoldingDisplayItem> = HashMap::new();
        let mut unfold_end = HashMap::new();
        let mut limit_line = 0;
        for item in &self.0 {
            if item.start.line < limit_line && limit_line > 0 {
                continue;
            }
            match item.status {
                FoldingRangeStatus::Fold => {
                    if let Some(line) = lines.info_for_line(item.start.line as usize)
                    {
                        folded.insert(
                            item.start.line,
                            FoldingDisplayItem {
                                position: item.start,
                                y: line.y as i32,
                                ty: FoldingDisplayType::Folded,
                            },
                        );
                    }
                    limit_line = item.end.line;
                }
                FoldingRangeStatus::Unfold => {
                    {
                        if let Some(line) =
                            lines.info_for_line(item.start.line as usize)
                        {
                            unfold_start.insert(
                                item.start.line,
                                FoldingDisplayItem {
                                    position: item.start,
                                    y: line.y as i32,
                                    ty: FoldingDisplayType::UnfoldStart,
                                },
                            );
                        }
                    }
                    {
                        if let Some(line) =
                            lines.info_for_line(item.end.line as usize)
                        {
                            unfold_end.insert(
                                item.end.line,
                                FoldingDisplayItem {
                                    position: item.end,
                                    y: line.y as i32,
                                    ty: FoldingDisplayType::UnfoldEnd,
                                },
                            );
                        }
                    }
                    limit_line = 0;
                }
            };
        }
        for (key, val) in unfold_end {
            unfold_start.insert(key, val);
        }
        for (key, val) in folded {
            unfold_start.insert(key, val);
        }
        unfold_start.into_iter().map(|x| x.1).collect()
    }

    pub fn update_ranges(&mut self, mut new: Vec<FoldingRange>) {
        let folded_range = self.get_folded_range();
        new.iter_mut().for_each(|x| folded_range.update_status(x));
        self.0 = new;
    }
}

impl FoldedRanges {
    pub fn visual_line(&self, line: usize) -> usize {
        let line = line as u32;
        for folded in &self.0 {
            if line <= folded.start.line {
                return line as usize;
            } else if folded.start.line < line && line <= folded.end.line {
                return folded.start.line as usize;
            }
        }
        line as usize
    }
    /// ??line: 该行是否被折叠。
    /// start_index: 下次检查的起始点
    pub fn contain_line(&self, start_index: usize, line: u32) -> (bool, usize) {
        if start_index >= self.0.len() {
            return (false, start_index);
        }
        let mut last_index = start_index;
        for range in self.0[start_index..].iter() {
            if range.start.line >= line {
                return (false, last_index);
                // todo range.end.line >= line
            } else if range.start.line < line && range.end.line >= line {
                return (true, last_index);
            } else if range.end.line < line {
                last_index += 1;
            }
        }
        (false, last_index)
    }

    pub fn contain_position(&self, position: Position) -> bool {
        self.0
            .iter()
            .any(|x| x.start <= position && x.end >= position)
    }

    pub fn update_status(&self, folding: &mut FoldingRange) {
        if self
            .0
            .iter()
            .any(|x| x.start == folding.start && x.end == folding.end)
        {
            folding.status = FoldingRangeStatus::Fold
        }
    }

    pub fn into_phantom_text(
        self,
        buffer: &Buffer,
        config: &LapceConfig,
        line: usize,
    ) -> Vec<PhantomText> {
        self.0
            .into_iter()
            .filter_map(|x| x.into_phantom_text(buffer, config, line as u32))
            .collect()
    }
}

fn get_offset(buffer: &Buffer, positon: Position) -> usize {
    buffer.offset_of_line(positon.line as usize) + positon.character as usize
}

#[derive(Debug, Clone)]
pub struct FoldedRange {
    pub start: Position,
    pub end: Position,
    pub collapsed_text: Option<String>,
}

impl FoldedRange {
    pub fn into_phantom_text(
        self,
        buffer: &Buffer,
        config: &LapceConfig,
        line: u32,
    ) -> Option<PhantomText> {
        // info!("line={line} start={:?} end={:?}", self.start, self.end);
        let same_line = self.end.line == self.start.line;
        if self.start.line == line {
            let start_char =
                buffer.char_at_offset(get_offset(buffer, self.start))?;
            let end_char =
                buffer.char_at_offset(get_offset(buffer, self.end) - 1)?;

            let mut text = String::new();
            text.push(start_char);
            text.push_str("...");
            text.push(end_char);
            let next_line = if same_line {
                None
            } else {
                Some(self.end.line as usize)
            };
            let start = self.start.character as usize;
            let len = if same_line {
                self.end.character as usize - start
            } else {
                let start_line_len = buffer.line_content(line as usize).len();
                start_line_len - start
            };
            Some(PhantomText {
                kind: PhantomTextKind::LineFoldedRang { next_line, len },
                col: start,
                text,
                affinity: None,
                fg: Some(config.color(LapceColor::INLAY_HINT_FOREGROUND)),
                font_size: Some(config.editor.inlay_hint_font_size()),
                bg: Some(config.color(LapceColor::INLAY_HINT_BACKGROUND)),
                under_line: None,
                final_col: start,
                line: line as usize,
                merge_col: start,
            })
        } else if self.end.line == line {
            let text = String::new();
            Some(PhantomText {
                kind: PhantomTextKind::LineFoldedRang {
                    next_line: None,
                    len: self.end.character as usize,
                },
                col: 0,
                text,
                affinity: None,
                fg: None,
                font_size: None,
                bg: None,
                under_line: None,
                final_col: 0,
                line: line as usize,
                merge_col: 0,
            })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct FoldingRange {
    pub start: Position,
    pub end: Position,
    pub status: FoldingRangeStatus,
    pub collapsed_text: Option<String>,
}

impl FoldingRange {
    pub fn from_lsp(value: lsp_types::FoldingRange) -> Self {
        let lsp_types::FoldingRange {
            start_line,
            start_character,
            end_line,
            end_character,
            collapsed_text,
            ..
        } = value;
        let status = FoldingRangeStatus::Unfold;
        Self {
            start: Position {
                line: start_line,
                character: start_character.unwrap_or_default(),
            },
            end: Position {
                line: end_line,
                character: end_character.unwrap_or_default(),
            },
            status,
            collapsed_text,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Copy)]
pub struct FoldingPosition {
    pub line: u32,
    pub character: Option<u32>,
    // pub kind: Option<FoldingRangeKind>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub enum FoldingRangeStatus {
    Fold,
    #[default]
    Unfold,
}

impl FoldingRangeStatus {
    pub fn click(&mut self) {
        match self {
            FoldingRangeStatus::Fold => {
                *self = FoldingRangeStatus::Unfold;
            }
            FoldingRangeStatus::Unfold => {
                *self = FoldingRangeStatus::Fold;
            }
        }
    }
    pub fn is_folded(&self) -> bool {
        *self == Self::Fold
    }
}
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct FoldingDisplayItem {
    pub position: Position,
    pub y: i32,
    pub ty: FoldingDisplayType,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum FoldingDisplayType {
    UnfoldStart,
    Folded,
    UnfoldEnd,
}

// impl FoldingDisplayItem {
//     pub fn position(&self) -> FoldingPosition {
//         self.position
//     }
// }

#[derive(Debug, Eq, PartialEq, Deserialize, Serialize, Clone, Hash, Copy)]
pub enum FoldingRangeKind {
    Comment,
    Imports,
    Region,
}

impl From<lsp_types::FoldingRangeKind> for FoldingRangeKind {
    fn from(value: lsp_types::FoldingRangeKind) -> Self {
        match value {
            lsp_types::FoldingRangeKind::Comment => FoldingRangeKind::Comment,
            lsp_types::FoldingRangeKind::Imports => FoldingRangeKind::Imports,
            lsp_types::FoldingRangeKind::Region => FoldingRangeKind::Region,
        }
    }
}
