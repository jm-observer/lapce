use floem::kurbo::Rect;
use floem::peniko::Color;
use floem::reactive::{
    batch, ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith,
};
use floem::text::{
    Attrs, AttrsList, FamilyOwned, LineHeightValue, TextLayout, Wrap, FONT_SYSTEM,
};
use lapce_xi_rope::{Interval, RopeDelta, Transformer};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::rc::Rc;
use std::sync::atomic::AtomicUsize;
use std::sync::{atomic, Arc};

use floem_editor_core::buffer::rope_text::RopeText;
use floem_editor_core::cursor::CursorAffinity;
use tracing::warn;

use crate::config::color::LapceColor;
use crate::config::LapceConfig;
use crate::doc::{DiagnosticData, Doc};
use crate::editor::gutter::{
    FoldingDisplayItem, FoldingDisplayType, FoldingRange, FoldingRanges,
};
use crate::editor::EditorViewKind;
use floem::views::editor::layout::TextLayoutLine;
use floem::views::editor::listener::Listener;
use floem::views::editor::phantom_text::{
    PhantomText, PhantomTextKind, PhantomTextLine, PhantomTextMultiLine,
};
use floem::views::editor::text::{Document, PreeditData, Styling, WrapMethod};
use floem::views::editor::view::{LineInfo, ScreenLines};
use floem::views::editor::visual_line::{
    LayoutEvent, RVLine, ResolvedWrap, VLine, VLineInfo,
};
use floem::views::editor::EditorStyle;
use floem_editor_core::buffer::Buffer;
use floem_editor_core::word::{get_char_property, CharClassification};
use itertools::Itertools;
use lapce_core::rope_text_pos::RopeTextPosition;
use lapce_core::style::line_styles;
use lapce_core::syntax::edit::SyntaxEdit;
use lapce_core::syntax::{BracketParser, Syntax};
use lapce_rpc::style::{LineStyle, Style};
use lapce_xi_rope::spans::{Spans, SpansBuilder};
use lsp_types::{DiagnosticSeverity, InlayHint, InlayHintLabel, Position};
use smallvec::SmallVec;

/// Minimum width that we'll allow the view to be wrapped at.
const MIN_WRAPPED_WIDTH: f32 = 100.0;

type LineStyles = HashMap<usize, Vec<LineStyle>>;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct OriginLine {
    line_index: usize,
    start_offset: usize,
}
#[allow(dead_code)]
#[derive(Clone)]
pub struct OriginFoldedLine {
    pub line_index: usize,
    // [origin_line_start..origin_line_end]
    pub origin_line_start: usize,
    pub origin_line_end: usize,
    origin_interval: Interval,
    pub text_layout: Arc<TextLayoutLine>,
}

impl Debug for OriginFoldedLine {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "OriginFoldedLine line_index={} origin_line_start={} origin_line_end={} origin_interval={}",
            self.line_index, self.origin_line_start, self.origin_line_end, self.origin_interval)
    }
}

#[derive(Clone, Debug)]
pub struct VisualLine {
    line_index: usize,
    origin_interval: Interval,
    origin_folded_line: usize,
    origin_folded_line_sub_index: usize,
}

impl VisualLine {
    pub fn rvline(&self) -> RVLine {
        RVLine {
            line: self.origin_folded_line,
            line_index: self.origin_folded_line_sub_index,
        }
    }

    pub fn vline(&self) -> VLine {
        VLine(self.line_index)
    }

    pub fn vline_info(&self) -> VLineInfo {
        let rvline = self.rvline();
        let vline = self.vline();
        let interval = self.origin_interval;
        // todo?
        let origin_line = self.origin_folded_line;
        VLineInfo {
            interval,
            rvline,
            origin_line,
            vline,
        }
    }
}

impl From<&VisualLine> for RVLine {
    fn from(value: &VisualLine) -> Self {
        value.rvline()
    }
}
impl From<&VisualLine> for VLine {
    fn from(value: &VisualLine) -> Self {
        value.vline()
    }
}
#[derive(Clone)]
pub struct DocLines {
    origin_lines: Vec<OriginLine>,
    origin_folded_lines: Vec<OriginFoldedLine>,
    visual_lines: Vec<VisualLine>,
    // pub font_sizes: Rc<EditorFontSizes>,
    // font_size_cache_id: FontSizeCacheId,
    // wrap: ResolvedWrap,
    pub layout_event: Listener<LayoutEvent>,
    max_width: f64,

    // editor: Editor
    pub inlay_hints: Option<Spans<InlayHint>>,
    pub completion_lens: Option<String>,
    pub completion_pos: (usize, usize),
    pub folding_ranges: FoldingRanges,
    // pub buffer: Buffer,
    pub diagnostics: DiagnosticData,

    /// Current inline completion text, if any.
    /// This will be displayed even on views that are not focused.
    /// (line, col)
    pub inline_completion: Option<(String, usize, usize)>,
    pub preedit: PreeditData,
    // tree-sitter
    pub syntax: Syntax,
    // lsp
    pub semantic_styles: Option<(Option<String>, Spans<Style>)>,
    pub parser: BracketParser,
    pub line_styles: LineStyles,
    pub editor_style: RwSignal<EditorStyle>,
    pub viewport: RwSignal<Rect>,
    pub config: ReadSignal<Arc<LapceConfig>>,
    pub buffer: RwSignal<Buffer>,
    pub screen_lines: RwSignal<ScreenLines>,
    pub kind: RwSignal<EditorViewKind>,
}

impl DocLines {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cx: Scope,
        diagnostics: DiagnosticData,
        syntax: Syntax,
        parser: BracketParser,
        viewport: RwSignal<Rect>,
        editor_style: RwSignal<EditorStyle>,
        config: ReadSignal<Arc<LapceConfig>>,
        buffer: RwSignal<Buffer>,
        screen_lines: RwSignal<ScreenLines>,
        kind: RwSignal<EditorViewKind>,
    ) -> Self {
        let mut lines = Self {
            // font_size_cache_id: id,
            layout_event: Listener::new_empty(cx), // font_size_cache_id: id,
            viewport,
            config,
            editor_style,
            origin_lines: vec![],
            origin_folded_lines: vec![],
            visual_lines: vec![],
            max_width: 0.0,

            inlay_hints: None,
            completion_pos: (0, 0),
            folding_ranges: Default::default(),
            // buffer: Buffer::new(""),
            diagnostics,
            completion_lens: None,
            inline_completion: None,
            preedit: PreeditData::new(cx),
            syntax,
            semantic_styles: None,
            parser,
            line_styles: Default::default(),
            buffer,
            screen_lines,
            kind,
        };
        lines.update_lines();
        lines
    }

    // pub fn update_cache_id(&mut self) {
    //     let current_id = self.font_sizes.cache_id();
    //     if current_id != self.font_size_cache_id {
    //         self.font_size_cache_id = current_id;
    //         self.update()
    //     }
    // }

    // pub fn update_font_sizes(&mut self, font_sizes: Rc<EditorFontSizes>) {
    //     self.font_sizes = font_sizes;
    //     self.update()
    // }

    fn clear(&mut self) {
        self.origin_lines.clear();
        self.origin_folded_lines.clear();
        self.visual_lines.clear();
        self.max_width = 0.0
    }

    fn update_parser(&mut self, buffer: &Buffer) {
        if self.syntax.styles.is_some() {
            self.parser.update_code(buffer, Some(&self.syntax));
        } else {
            self.parser.update_code(buffer, None);
        }
    }

    pub fn update_lines(&mut self) {
        let buffer = self.buffer.get_untracked();
        self.update_lines_with_buffer(&buffer);
        // todo update screen_lines
    }
    // return do_update
    fn update_lines_with_buffer(&mut self, buffer: &Buffer) {
        self.clear();
        warn!("update_lines_with_buffer");
        let last_line = buffer.last_line();
        // self.update_parser(buffer);
        let mut current_line = 0;
        let mut origin_folded_line_index = 0;
        let mut visual_line_index = 0;
        while current_line <= last_line {
            let text_layout = self.new_text_layout(current_line, buffer);
            let origin_line_start = text_layout.phantom_text.line;
            let origin_line_end = text_layout.phantom_text.last_line;

            let width = text_layout.text.size().width;
            if width > self.max_width {
                self.max_width = width;
            }

            for origin_line in origin_line_start..=origin_line_end {
                self.origin_lines.push(OriginLine {
                    line_index: origin_line,
                    start_offset: buffer.offset_of_line(origin_line),
                });
            }

            let mut visual_offset_start = 0;
            let mut visual_offset_end;
            // [visual_offset_start..visual_offset_end)
            for (origin_folded_line_sub_index, layout) in
                text_layout.text.line_layout().iter().enumerate()
            {
                visual_offset_end = visual_offset_start + layout.glyphs.len();

                let offset_info = text_layout
                    .phantom_text
                    .origin_position_of_final_col(visual_offset_start);
                let origin_interval_start =
                    buffer.offset_of_line_col(offset_info.0, offset_info.1);

                let offset_info = text_layout
                    .phantom_text
                    .origin_position_of_final_col(visual_offset_end);
                let origin_interval_end =
                    buffer.offset_of_line_col(offset_info.0, offset_info.1);
                let origin_interval = Interval {
                    start: origin_interval_start,
                    end: origin_interval_end,
                };

                self.visual_lines.push(VisualLine {
                    line_index: visual_line_index,
                    origin_interval,
                    origin_folded_line: origin_folded_line_index,
                    origin_folded_line_sub_index,
                });

                visual_offset_start = visual_offset_end;
                visual_line_index += 1;
            }

            let origin_interval = Interval {
                start: buffer.offset_of_line(origin_line_start),
                end: buffer.offset_of_line(origin_line_end + 1),
            };
            self.origin_folded_lines.push(OriginFoldedLine {
                line_index: origin_folded_line_index,
                origin_line_start,
                origin_line_end,
                origin_interval,
                text_layout,
            });

            current_line = origin_line_end + 1;
            origin_folded_line_index += 1;
        }
    }

    // pub fn wrap(&self, viewport: Rect, es: &EditorStyle) -> ResolvedWrap {
    //     match es.wrap_method() {
    //         WrapMethod::None => ResolvedWrap::None,
    //         WrapMethod::EditorWidth => {
    //             ResolvedWrap::Width((viewport.width() as f32).max(MIN_WRAPPED_WIDTH))
    //         }
    //         WrapMethod::WrapColumn { .. } => todo!(),
    //         WrapMethod::WrapWidth { width } => ResolvedWrap::Width(width),
    //     }
    // }

    /// Set the wrapping style
    ///
    /// Does nothing if the wrapping style is the same as the current one.
    /// Will trigger a clear of the text layouts if the wrapping style is different.
    // pub fn set_wrap(&mut self, wrap: ResolvedWrap, _editor: &Editor) {
    //     if wrap == self.wrap {
    //         return;
    //     }
    //     self.wrap = wrap;
    //     // self.update(editor);
    // }

    pub fn max_width(&self) -> f64 {
        self.max_width
    }

    /// ~~视觉~~行的text_layout信息
    pub fn text_layout_of_visual_line(&self, line: usize) -> Arc<TextLayoutLine> {
        self.origin_folded_lines[self.visual_lines[line].origin_folded_line]
            .text_layout
            .clone()
    }

    // 原始行的第一个视觉行。原始行可能会有多个视觉行
    pub fn start_visual_line_of_origin_line(
        &self,
        origin_line: usize,
    ) -> &VisualLine {
        let folded_line = self.folded_line_of_origin_line(origin_line);
        self.start_visual_line_of_folded_line(folded_line.line_index)
    }

    pub fn start_visual_line_of_folded_line(
        &self,
        origin_folded_line: usize,
    ) -> &VisualLine {
        for visual_line in &self.visual_lines {
            if visual_line.origin_folded_line == origin_folded_line {
                return visual_line;
            }
        }
        panic!()
    }

    pub fn folded_line_of_origin_line(
        &self,
        origin_line: usize,
    ) -> &OriginFoldedLine {
        for folded_line in &self.origin_folded_lines {
            if folded_line.origin_line_start <= origin_line
                && origin_line <= folded_line.origin_line_end
            {
                return folded_line;
            }
        }
        panic!()
    }

    pub fn visual_line_of_folded_line_and_sub_index(
        &self,
        origin_folded_line: usize,
        sub_index: usize,
    ) -> &VisualLine {
        for visual_line in &self.visual_lines {
            if visual_line.origin_folded_line == origin_folded_line
                && visual_line.origin_folded_line_sub_index == sub_index
            {
                return visual_line;
            }
        }
        panic!()
    }

    pub fn last_visual_line(&self) -> &VisualLine {
        &self.visual_lines[self.visual_lines.len() - 1]
    }

    /// 原始字符所在的视觉行，以及行的偏移位置和是否是最后一个字符
    pub fn visual_line_of_offset(
        &self,
        origin_line: usize,
        offset: usize,
        _affinity: CursorAffinity,
    ) -> (VLineInfo, usize, bool) {
        // 位于的原始行，以及在原始行的起始offset
        // let (origin_line, offset_of_line) = self.font_sizes.doc.with_untracked(|x| {
        //     let text = x.text();
        //     let origin_line = text.line_of_offset(offset);
        //     let origin_line_start_offset = text.offset_of_line(origin_line);
        //     (origin_line, origin_line_start_offset)
        // });
        // let mut offset = offset - offset_of_line;
        let folded_line = self.folded_line_of_origin_line(origin_line);
        let mut final_offset = folded_line
            .text_layout
            .phantom_text
            .final_col_of_col(origin_line, offset, false);
        let folded_line_layout = folded_line.text_layout.text.line_layout();
        let mut sub_line_index = folded_line_layout.len() - 1;
        let mut last_char = false;
        for (index, sub_line) in folded_line_layout.iter().enumerate() {
            if final_offset < sub_line.glyphs.len() {
                sub_line_index = index;
                last_char = final_offset == sub_line.glyphs.len() - 1;
                break;
            } else {
                final_offset -= sub_line.glyphs.len();
            }
        }
        let visual_line = self.visual_line_of_folded_line_and_sub_index(
            folded_line.line_index,
            sub_line_index,
        );

        (visual_line.vline_info(), final_offset, last_char)
    }

    pub fn vline_infos(&self, start: usize, end: usize) -> Vec<VLineInfo<VLine>> {
        let start = start.min(self.visual_lines.len() - 1);
        let end = end.min(self.visual_lines.len() - 1);

        let mut vline_infos = Vec::with_capacity(end - start + 1);
        for index in start..=end {
            vline_infos.push(self.visual_lines[index].vline_info());
        }
        vline_infos
    }

    pub fn first_vline_info(&self) -> VLineInfo<VLine> {
        self.visual_lines[0].vline_info()
    }

    fn phantom_text(
        &self,
        _: &EditorStyle,
        line: usize,
        config: &LapceConfig,
        buffer: &Buffer,
    ) -> PhantomTextLine {
        let (start_offset, end_offset) =
            (buffer.offset_of_line(line), buffer.offset_of_line(line + 1));

        let origin_text_len = end_offset - start_offset;
        // lsp返回的字符包括换行符，现在长度不考虑，后续会有问题
        // let line_ending = buffer.line_ending().get_chars().len();
        // if origin_text_len >= line_ending {
        //     origin_text_len -= line_ending;
        // }
        // if line == 8 {
        //     tracing::info!("start_offset={start_offset} end_offset={end_offset} line_ending={line_ending} origin_text_len={origin_text_len}");
        // }

        let folded_ranges =
            self.folding_ranges.get_folded_range_by_line(line as u32);

        // If hints are enabled, and the hints field is filled, then get the hints for this line
        // and convert them into PhantomText instances
        let hints = config
            .editor
            .enable_inlay_hints
            .then_some(())
            .and(self.inlay_hints.as_ref())
            .map(|hints| hints.iter_chunks(start_offset..end_offset))
            .into_iter()
            .flatten()
            .filter(|(interval, hint)| {
                interval.start >= start_offset
                    && interval.start < end_offset
                    && !folded_ranges.contain_position(hint.position)
            })
            .map(|(interval, inlay_hint)| {
                let (col, affinity) = {
                    let mut cursor =
                        lapce_xi_rope::Cursor::new(buffer.text(), interval.start);

                    let next_char = cursor.peek_next_codepoint();
                    let prev_char = cursor.prev_codepoint();

                    let mut affinity = None;
                    if let Some(prev_char) = prev_char {
                        let c = get_char_property(prev_char);
                        if c == CharClassification::Other {
                            affinity = Some(CursorAffinity::Backward)
                        } else if matches!(
                            c,
                            CharClassification::Lf
                                | CharClassification::Cr
                                | CharClassification::Space
                        ) {
                            affinity = Some(CursorAffinity::Forward)
                        }
                    };
                    if affinity.is_none() {
                        if let Some(next_char) = next_char {
                            let c = get_char_property(next_char);
                            if c == CharClassification::Other {
                                affinity = Some(CursorAffinity::Forward)
                            } else if matches!(
                                c,
                                CharClassification::Lf
                                    | CharClassification::Cr
                                    | CharClassification::Space
                            ) {
                                affinity = Some(CursorAffinity::Backward)
                            }
                        }
                    }

                    let (_, col) = buffer.offset_to_line_col(interval.start);
                    (col, affinity)
                };
                let mut text = match &inlay_hint.label {
                    InlayHintLabel::String(label) => label.to_string(),
                    InlayHintLabel::LabelParts(parts) => {
                        parts.iter().map(|p| &p.value).join("")
                    }
                };
                match (text.starts_with(':'), text.ends_with(':')) {
                    (true, true) => {
                        text.push(' ');
                    }
                    (true, false) => {
                        text.push(' ');
                    }
                    (false, true) => {
                        text = format!(" {} ", text);
                    }
                    (false, false) => {
                        text = format!(" {}", text);
                    }
                }
                PhantomText {
                    kind: PhantomTextKind::InlayHint,
                    col,
                    text,
                    affinity,
                    fg: Some(config.color(LapceColor::INLAY_HINT_FOREGROUND)),
                    // font_family: Some(config.editor.inlay_hint_font_family()),
                    font_size: Some(config.editor.inlay_hint_font_size()),
                    bg: Some(config.color(LapceColor::INLAY_HINT_BACKGROUND)),
                    under_line: None,
                    final_col: col,
                    line,
                    merge_col: col,
                }
            });
        // You're quite unlikely to have more than six hints on a single line
        // this later has the diagnostics added onto it, but that's still likely to be below six
        // overall.
        let mut text: SmallVec<[PhantomText; 6]> = hints.collect();

        // If error lens is enabled, and the diagnostics field is filled, then get the diagnostics
        // that end on this line which have a severity worse than HINT and convert them into
        // PhantomText instances

        let mut diag_text: SmallVec<[PhantomText; 6]> = config
            .editor
            .enable_error_lens
            .then_some(())
            .map(|_| self.diagnostics.diagnostics_span.get_untracked())
            .map(|diags| {
                diags
                    .iter_chunks(start_offset..end_offset)
                    .filter_map(|(iv, diag)| {
                        let end = iv.end();
                        let end_line = buffer.line_of_offset(end);
                        if end_line == line
                            && diag.severity < Some(DiagnosticSeverity::HINT)
                            && !folded_ranges.contain_position(diag.range.start)
                            && !folded_ranges.contain_position(diag.range.end)
                        {
                            let fg = {
                                let severity = diag
                                    .severity
                                    .unwrap_or(DiagnosticSeverity::WARNING);
                                let theme_prop = if severity
                                    == DiagnosticSeverity::ERROR
                                {
                                    LapceColor::ERROR_LENS_ERROR_FOREGROUND
                                } else if severity == DiagnosticSeverity::WARNING {
                                    LapceColor::ERROR_LENS_WARNING_FOREGROUND
                                } else {
                                    // information + hint (if we keep that) + things without a severity
                                    LapceColor::ERROR_LENS_OTHER_FOREGROUND
                                };

                                config.color(theme_prop)
                            };

                            let text = if config.editor.only_render_error_styling {
                                "".to_string()
                            } else if config.editor.error_lens_multiline {
                                format!("    {}", diag.message)
                            } else {
                                format!("    {}", diag.message.lines().join(" "))
                            };
                            Some(PhantomText {
                                kind: PhantomTextKind::Diagnostic,
                                col: end_offset - start_offset,
                                affinity: Some(CursorAffinity::Backward),
                                text,
                                fg: Some(fg),
                                font_size: Some(
                                    config.editor.error_lens_font_size(),
                                ),
                                bg: None,
                                under_line: None,
                                final_col: end_offset - start_offset,
                                line,
                                merge_col: end_offset - start_offset,
                            })
                        } else {
                            None
                        }
                    })
                    .collect::<SmallVec<[PhantomText; 6]>>()
            })
            .unwrap_or_default();

        text.append(&mut diag_text);

        let (completion_line, completion_col) = self.completion_pos;
        let completion_text = config
            .editor
            .enable_completion_lens
            .then_some(())
            .and(self.completion_lens.as_ref())
            // TODO: We're probably missing on various useful completion things to include here!
            .filter(|_| {
                line == completion_line
                    && !folded_ranges.contain_position(Position {
                        line: completion_line as u32,
                        character: completion_col as u32,
                    })
            })
            .map(|completion| PhantomText {
                kind: PhantomTextKind::Completion,
                col: completion_col,
                text: completion.clone(),
                fg: Some(config.color(LapceColor::COMPLETION_LENS_FOREGROUND)),
                font_size: Some(config.editor.completion_lens_font_size()),
                affinity: Some(CursorAffinity::Backward),
                // font_family: Some(config.editor.completion_lens_font_family()),
                bg: None,
                under_line: None,
                final_col: completion_col,
                line,
                merge_col: completion_col,
                // TODO: italics?
            });
        if let Some(completion_text) = completion_text {
            text.push(completion_text);
        }

        // TODO: don't display completion lens and inline completion at the same time
        // and/or merge them so that they can be shifted between like multiple inline completions
        // can
        // let (inline_completion_line, inline_completion_col) =
        //     self.inline_completion_pos;
        let inline_completion_text = config
            .editor
            .enable_inline_completion
            .then_some(())
            .and(self.inline_completion.as_ref())
            .filter(|(_, inline_completion_line, inline_completion_col)| {
                line == *inline_completion_line
                    && !folded_ranges.contain_position(Position {
                        line: *inline_completion_line as u32,
                        character: *inline_completion_col as u32,
                    })
            })
            .map(|(completion, _, inline_completion_col)| {
                PhantomText {
                    kind: PhantomTextKind::Completion,
                    col: *inline_completion_col,
                    text: completion.clone(),
                    affinity: Some(CursorAffinity::Backward),
                    fg: Some(config.color(LapceColor::COMPLETION_LENS_FOREGROUND)),
                    font_size: Some(config.editor.completion_lens_font_size()),
                    // font_family: Some(config.editor.completion_lens_font_family()),
                    bg: None,
                    under_line: None,
                    final_col: *inline_completion_col,
                    line,
                    merge_col: *inline_completion_col,
                    // TODO: italics?
                }
            });
        if let Some(inline_completion_text) = inline_completion_text {
            text.push(inline_completion_text);
        }

        // todo filter by folded?
        if let Some(preedit) = preedit_phantom(
            &self.preedit,
            buffer,
            Some(config.color(LapceColor::EDITOR_FOREGROUND)),
            line,
        ) {
            text.push(preedit)
        }
        text.extend(folded_ranges.into_phantom_text(buffer, config, line));

        PhantomTextLine::new(line, origin_text_len, text)
    }

    fn new_text_layout(
        &mut self,
        line: usize,
        buffer: &Buffer,
    ) -> Arc<TextLayoutLine> {
        // TODO: we could share text layouts between different editor views given some knowledge of
        // their wrapping
        let es = self.editor_style.get_untracked();
        let viewport = self.viewport.get_untracked();
        let config: Arc<LapceConfig> = self.config.get_untracked();

        let mut line_content = String::new();
        // Get the line content with newline characters replaced with spaces
        // and the content without the newline characters
        // TODO: cache or add some way that text layout is created to auto insert the spaces instead
        // though we immediately combine with phantom text so that's a thing.
        let line_content_original = buffer.line_content(line);
        let mut font_system = FONT_SYSTEM.lock();
        push_strip_suffix(&line_content_original, &mut line_content);

        let family = Cow::Owned(
            FamilyOwned::parse_list(&config.editor.font_family).collect(),
        );
        let font_size = config.editor.font_size();
        let line_height = config.editor.line_height();

        let attrs = Attrs::new()
            .color(es.ed_text_color())
            .family(&family)
            .font_size(font_size as f32)
            .line_height(LineHeightValue::Px(line_height as f32));

        let phantom_text = self.phantom_text(&es, line, &config, buffer);
        let mut collapsed_line_col = phantom_text.folded_line();
        let multi_styles: Vec<(usize, usize, Color, Attrs)> = self
            .line_styles(line, buffer, config.as_ref())
            .into_iter()
            .map(|(start, end, color)| (start, end, color, attrs))
            .collect();

        let mut phantom_text = PhantomTextMultiLine::new(phantom_text);
        let mut attrs_list = AttrsList::new(attrs);
        for (start, end, color, attrs) in multi_styles.into_iter() {
            let (Some(start), Some(end)) =
                (phantom_text.col_at(start), phantom_text.col_at(end))
            else {
                continue;
            };
            attrs_list.add_span(start..end, attrs.color(color));
        }

        while let Some(collapsed_line) = collapsed_line_col.take() {
            push_strip_suffix(
                &buffer.line_content(collapsed_line),
                &mut line_content,
            );

            let offset_col = phantom_text.final_text_len();
            let attrs = Attrs::new()
                .color(es.ed_text_color())
                .family(&family)
                .font_size(font_size as f32)
                .line_height(LineHeightValue::Px(line_height as f32));
            // let (next_phantom_text, collapsed_line_content, styles, next_collapsed_line_col)
            //     = calcuate_line_text_and_style(collapsed_line, &next_line_content, style.clone(), edid, &es, doc.clone(), offset_col, attrs);

            let next_phantom_text =
                self.phantom_text(&es, collapsed_line, &config, buffer);
            collapsed_line_col = next_phantom_text.folded_line();
            let styles: Vec<(usize, usize, Color, Attrs)> = self
                .line_styles(collapsed_line, buffer, config.as_ref())
                .into_iter()
                .map(|(start, end, color)| {
                    (start + offset_col, end + offset_col, color, attrs)
                })
                .collect();

            for (start, end, color, attrs) in styles.into_iter() {
                let (Some(start), Some(end)) =
                    (phantom_text.col_at(start), phantom_text.col_at(end))
                else {
                    continue;
                };
                attrs_list.add_span(start..end, attrs.color(color));
            }
            phantom_text.merge(next_phantom_text);
        }
        let phantom_color = es.phantom_color();
        phantom_text.add_phantom_style(
            &mut attrs_list,
            attrs,
            font_size,
            phantom_color,
        );

        // if line == 1 {
        //     tracing::info!("start");
        //     for (range, attr) in attrs_list.spans() {
        //         tracing::info!("{range:?} {attr:?}");
        //     }
        //     tracing::info!("");
        // }

        // tracing::info!("{line} {line_content}");
        // TODO: we could move tab width setting to be done by the document
        let final_line_content = phantom_text.final_line_content(&line_content);
        let mut text_layout = TextLayout::new_with_font_system(
            line,
            &final_line_content,
            attrs_list,
            &mut font_system,
        );
        drop(font_system);
        // text_layout.set_tab_width(style.tab_width(edid, line));

        // dbg!(self.editor_style.with(|s| s.wrap_method()));
        match es.wrap_method() {
            WrapMethod::None => {}
            WrapMethod::EditorWidth => {
                let width = viewport.width();
                text_layout.set_wrap(Wrap::WordOrGlyph);
                text_layout.set_size(width as f32, f32::MAX);
            }
            WrapMethod::WrapWidth { width } => {
                text_layout.set_wrap(Wrap::WordOrGlyph);
                text_layout.set_size(width, f32::MAX);
            }
            // TODO:
            WrapMethod::WrapColumn { .. } => {}
        }

        // let whitespaces = Self::new_whitespace_layout(
        //     &line_content_original,
        //     &text_layout,
        //     &phantom_text,
        //     es.render_whitespace(),
        // );
        // tracing::info!("line={line} {:?}", whitespaces);
        let indent_line = self.indent_line(line, &line_content_original, buffer);

        // let indent = if indent_line != line {
        //     // TODO: This creates the layout if it isn't already cached, but it doesn't cache the
        //     // result because the current method of managing the cache is not very smart.
        //     let layout = self.try_get_text_layout(indent_line).unwrap_or_else(|| {
        //         self.new_text_layout(
        //             indent_line,
        //             style.font_size(edid, indent_line),
        //             self.lines.wrap(),
        //         )
        //     });
        //     layout.indent + 1.0
        // } else {
        //     let offset = text.first_non_blank_character_on_line(indent_line);
        //     let (_, col) = text.offset_to_line_col(offset);
        //     text_layout.hit_position(col).point.x
        // };
        let offset = buffer.first_non_blank_character_on_line(indent_line);
        let (_, col) = buffer.offset_to_line_col(offset);
        let indent = text_layout.hit_position(col).point.x;

        let layout_line = TextLayoutLine {
            text: text_layout,
            extra_style: Vec::new(),
            whitespaces: None,
            indent,
            phantom_text,
        };
        // todo 下划线等？
        // let extra_style = style.apply_layout_styles(&layout_line.text, &layout_line.phantom_text, 0);
        //
        // layout_line.extra_style.clear();
        // layout_line.extra_style.extend(extra_style);

        Arc::new(layout_line)
    }

    pub fn update_inlay_hints(&mut self, delta: &RopeDelta) {
        if let Some(hints) = self.inlay_hints.as_mut() {
            hints.apply_shape(delta);
        }
        self.update_lines();
    }
    pub fn set_inlay_hints(&mut self, inlay_hint: Spans<InlayHint>) {
        self.inlay_hints = Some(inlay_hint);
        self.update_lines();
    }

    pub fn set_completion_lens(
        &mut self,
        completion_lens: String,
        line: usize,
        col: usize,
    ) {
        self.completion_lens = Some(completion_lens);
        self.completion_pos = (line, col);
        self.update_lines();
    }

    pub fn update_folding_item(&mut self, item: FoldingDisplayItem) {
        match item.ty {
            FoldingDisplayType::UnfoldStart | FoldingDisplayType::Folded => {
                self.folding_ranges.0.iter_mut().find_map(|range| {
                    if range.start == item.position {
                        range.status.click();
                        Some(())
                    } else {
                        None
                    }
                });
            }
            FoldingDisplayType::UnfoldEnd => {
                self.folding_ranges.0.iter_mut().find_map(|range| {
                    if range.end == item.position {
                        range.status.click();
                        Some(())
                    } else {
                        None
                    }
                });
            }
        }
        self.update_lines();
    }

    pub fn update_folding_ranges(&mut self, new: Vec<FoldingRange>) {
        self.folding_ranges.update_ranges(new);
        self.update_lines();
    }

    pub fn clear_completion_lens(&mut self) {
        self.completion_lens = None;
        self.update_lines();
    }

    pub fn update_completion_lens(&mut self, delta: &RopeDelta) {
        let Some(completion) = &mut self.completion_lens else {
            return;
        };
        let buffer = self.buffer.get_untracked();
        let (line, col) = self.completion_pos;
        let offset = buffer.offset_of_line_col(line, col);

        // If the edit is easily checkable + updateable from, then we alter the lens' text.
        // In normal typing, if we didn't do this, then the text would jitter forward and then
        // backwards as the completion lens is updated.
        // TODO: this could also handle simple deletion, but we don't currently keep track of
        // the past copmletion lens string content in the field.
        if delta.as_simple_insert().is_some() {
            let (iv, new_len) = delta.summary();
            if iv.start() == iv.end()
                && iv.start() == offset
                && new_len <= completion.len()
            {
                // Remove the # of newly inserted characters
                // These aren't necessarily the same as the characters literally in the
                // text, but the completion will be updated when the completion widget
                // receives the update event, and it will fix this if needed.
                // TODO: this could be smarter and use the insert's content
                self.completion_lens = Some(completion[new_len..].to_string());
            }
        }

        // Shift the position by the rope delta
        let mut transformer = Transformer::new(delta);

        let new_offset = transformer.transform(offset, true);
        let new_pos = buffer.offset_to_line_col(new_offset);

        self.completion_pos = new_pos;
        self.update_lines_with_buffer(&buffer);
    }
    pub fn init_diagnostics(&mut self) {
        let buffer = self.buffer.get_untracked();
        self.init_diagnostics_with_buffer(&buffer);
        self.update_lines_with_buffer(&buffer);
    }
    /// init by lsp
    fn init_diagnostics_with_buffer(&self, buffer: &Buffer) {
        let len = buffer.len();
        let diagnostics = self.diagnostics.diagnostics.get_untracked();
        let mut span = SpansBuilder::new(len);
        for diag in diagnostics.iter() {
            let start = buffer.offset_of_position(&diag.range.start);
            let end = buffer.offset_of_position(&diag.range.end);
            span.add_span(Interval::new(start, end), diag.to_owned());
        }
        let span = span.build();
        self.diagnostics.diagnostics_span.set(span);
    }

    pub fn update_diagnostics(&mut self, delta: &RopeDelta) {
        if self
            .diagnostics
            .diagnostics
            .with_untracked(|d| d.is_empty())
        {
            return;
        }

        self.diagnostics.diagnostics_span.update(|diagnostics| {
            diagnostics.apply_shape(delta);
        });
        self.update_lines();
    }

    pub fn set_inline_completion(
        &mut self,
        inline_completion: String,
        line: usize,
        col: usize,
    ) {
        self.inline_completion = Some((inline_completion, line, col));
        self.update_lines();
    }

    pub fn clear_inline_completion(&mut self) {
        self.inline_completion = None;
        self.update_lines();
    }

    pub fn update_inline_completion(&mut self, delta: &RopeDelta) {
        let Some((completion, ..)) = self.inline_completion.take() else {
            return;
        };
        let buffer = self.buffer.get_untracked();

        let (line, col) = self.completion_pos;
        let offset = buffer.offset_of_line_col(line, col);

        // Shift the position by the rope delta
        let mut transformer = Transformer::new(delta);

        let new_offset = transformer.transform(offset, true);
        let new_pos = buffer.offset_to_line_col(new_offset);

        if delta.as_simple_insert().is_some() {
            let (iv, new_len) = delta.summary();
            if iv.start() == iv.end()
                && iv.start() == offset
                && new_len <= completion.len()
            {
                // Remove the # of newly inserted characters
                // These aren't necessarily the same as the characters literally in the
                // text, but the completion will be updated when the completion widget
                // receives the update event, and it will fix this if needed.
                self.inline_completion =
                    Some((completion[new_len..].to_string(), new_pos.0, new_pos.1));
            }
        } else {
            self.inline_completion = Some((completion, new_pos.0, new_pos.1));
        }
        self.update_lines_with_buffer(&buffer);
    }

    pub fn set_syntax(&mut self, syntax: Syntax) {
        self.syntax = syntax;
        if self.semantic_styles.is_none() {
            self.line_styles.clear();
        }
        let buffer = self.buffer.get_untracked();
        self.update_parser(&buffer);
        self.update_lines_with_buffer(&buffer);
    }

    pub fn update_styles(&mut self, delta: &RopeDelta) {
        if let Some(styles) = self.syntax.styles.as_mut() {
            styles.apply_shape(delta);
        }
        self.syntax.lens.apply_delta(delta);
        if let Some(styles) = &mut self.semantic_styles {
            styles.1.apply_shape(delta);
        }
        self.update_lines()
    }

    pub fn trigger_syntax_change(
        &mut self,
        _edits: Option<SmallVec<[SyntaxEdit; 3]>>,
    ) {
        self.syntax.cancel_flag.store(1, atomic::Ordering::Relaxed);
        self.syntax.cancel_flag = Arc::new(AtomicUsize::new(0));
        self.update_lines();
    }

    fn styles(&self) -> Option<Spans<Style>> {
        if let Some(semantic_styles) = &self.semantic_styles {
            Some(semantic_styles.1.clone())
        } else {
            self.syntax.styles.clone()
        }
    }

    pub fn on_update_buffer(&mut self) {
        let buffer = self.buffer.get_untracked();
        if self.syntax.styles.is_some() {
            self.parser.update_code(&buffer, Some(&self.syntax));
        } else {
            self.parser.update_code(&buffer, None);
        }
        self.init_diagnostics_with_buffer(&buffer);
        self.update_lines_with_buffer(&buffer);
        // self.do_bracket_colorization(&buffer);
    }

    // fn do_bracket_colorization(&mut self, buffer: &Buffer) {
    //     if self.parser.active {
    //         if self.syntax.styles.is_some() {
    //             self.parser.update_code(&buffer, Some(&self.syntax));
    //         } else {
    //             self.parser.update_code(&buffer, None);
    //         }
    //     }
    // }

    fn line_styles(
        &mut self,
        line: usize,
        buffer: &Buffer,
        config: &LapceConfig,
    ) -> Vec<(usize, usize, Color)> {
        let mut styles: Vec<(usize, usize, Color)> = self
            .line_style(line, buffer)
            .iter()
            .filter_map(|line_style| {
                if let Some(fg_color) = line_style.style.fg_color.as_ref() {
                    if let Some(fg_color) = config.style_color(fg_color) {
                        return Some((line_style.start, line_style.end, fg_color));
                    }
                }
                None
            })
            .collect();
        if let Some(bracket_styles) = self.parser.bracket_pos.get(&line) {
            let mut bracket_styles = bracket_styles
                .iter()
                .filter_map(|bracket_style| {
                    if let Some(fg_color) = bracket_style.style.fg_color.as_ref() {
                        if let Some(fg_color) = config.style_color(fg_color) {
                            return Some((
                                bracket_style.start,
                                bracket_style.end,
                                fg_color,
                            ));
                        }
                    }
                    None
                })
                .collect();
            styles.append(&mut bracket_styles);
        }
        styles
    }

    fn line_style(&mut self, line: usize, buffer: &Buffer) -> Vec<LineStyle> {
        let styles = self.styles();
        self.line_styles
            .entry(line)
            .or_insert_with(|| {
                let line_styles = styles
                    .map(|styles| {
                        let text = buffer.text();
                        line_styles(text, line, &styles)
                    })
                    .unwrap_or_default();
                line_styles
            })
            .clone()
    }

    fn indent_line(
        &self,
        line: usize,
        line_content: &str,
        buffer: &Buffer,
    ) -> usize {
        if line_content.trim().is_empty() {
            let offset = buffer.offset_of_line(line);
            if let Some(offset) = self.syntax.parent_offset(offset) {
                return buffer.line_of_offset(offset);
            }
        }
        line
    }

    pub(crate) fn compute_screen_lines(&self) {
        // TODO: this should probably be a get since we need to depend on line-height
        // let doc_lines = doc.doc_lines.get_untracked();
        let config = self.config.get_untracked();
        let line_height = config.editor.line_height();
        let view_kind = self.kind.get_untracked();
        let base = self.viewport.get_untracked();

        let (y0, y1) = (base.y0, base.y1);
        // Get the start and end (visual) lines that are visible in the viewport
        let min_val = (y0 / line_height as f64).floor() as usize;
        let min_vline = VLine(min_val);
        let max_val = (y1 / line_height as f64).floor() as usize;

        // let cache_rev = doc.cache_rev.get();
        // lines.check_cache_rev(cache_rev);
        // TODO(minor): we don't really need to depend on various subdetails that aren't affecting how
        // the screen lines are set up, like the title of a scratch document.
        // doc.content.track();
        // doc.loaded.track();

        match view_kind {
            EditorViewKind::Normal => {
                let mut rvlines = Vec::new();
                let mut info = HashMap::new();

                let vline_infos = self.vline_infos(min_val, max_val);

                for vline_info in vline_infos {
                    rvlines.push(vline_info.rvline);
                    let y_idx = min_vline.get() + rvlines.len();
                    let vline_y = y_idx * line_height;
                    let line_y =
                        vline_y - vline_info.rvline.line_index * line_height;

                    // Add the information to make it cheap to get in the future.
                    // This y positions are shifted by the baseline y0
                    info.insert(
                        vline_info.rvline,
                        LineInfo {
                            y: line_y as f64 - y0,
                            vline_y: vline_y as f64 - y0,
                            vline_info,
                        },
                    );
                }
                self.screen_lines.update(|x| {
                    x.lines = rvlines;
                    x.info = Rc::new(info);
                    x.diff_sections = None;
                });
            }
            EditorViewKind::Diff(_diff_info) => {
                // TODO: let lines in diff view be wrapped, possibly screen_lines should be impl'd
                // on DiffEditorData
                todo!()
                // let mut y_idx = 0;
                // let mut rvlines = Vec::new();
                // let mut info = HashMap::new();
                // let mut diff_sections = Vec::new();
                // let mut last_change: Option<&DiffLines> = None;
                // let mut changes = diff_info.changes.iter().peekable();
                // let is_right = diff_info.is_right;
                //
                // let line_y = |info: VLineInfo<()>, vline_y: usize| -> usize {
                //     vline_y.saturating_sub(info.rvline.line_index * line_height)
                // };
                //
                // while let Some(change) = changes.next() {
                //     match (is_right, change) {
                //         (true, DiffLines::Left(range)) => {
                //             if let Some(DiffLines::Right(_)) = changes.peek() {
                //             } else {
                //                 let len = range.len();
                //                 diff_sections.push(DiffSection {
                //                     y_idx,
                //                     height: len,
                //                     kind: DiffSectionKind::NoCode,
                //                 });
                //                 y_idx += len;
                //             }
                //         }
                //         (false, DiffLines::Right(range)) => {
                //             let len = if let Some(DiffLines::Left(r)) = last_change {
                //                 range.len() - r.len().min(range.len())
                //             } else {
                //                 range.len()
                //             };
                //             if len > 0 {
                //                 diff_sections.push(DiffSection {
                //                     y_idx,
                //                     height: len,
                //                     kind: DiffSectionKind::NoCode,
                //                 });
                //                 y_idx += len;
                //             }
                //         }
                //         (true, DiffLines::Right(range))
                //         | (false, DiffLines::Left(range)) => {
                //             // TODO: count vline count in the range instead
                //             let height = range.len();
                //
                //             diff_sections.push(DiffSection {
                //                 y_idx,
                //                 height,
                //                 kind: if is_right {
                //                     DiffSectionKind::Added
                //                 } else {
                //                     DiffSectionKind::Removed
                //                 },
                //             });
                //
                //             let initial_y_idx = y_idx;
                //             // Mopve forward by the count given.
                //             y_idx += height;
                //
                //             if y_idx < min_vline.get() {
                //                 if is_right {
                //                     if let Some(DiffLines::Left(r)) = last_change {
                //                         // TODO: count vline count in the other editor since this is skipping an amount dependent on those vlines
                //                         let len = r.len() - r.len().min(range.len());
                //                         if len > 0 {
                //                             diff_sections.push(DiffSection {
                //                                 y_idx,
                //                                 height: len,
                //                                 kind: DiffSectionKind::NoCode,
                //                             });
                //                             y_idx += len;
                //                         }
                //                     };
                //                 }
                //                 last_change = Some(change);
                //                 continue;
                //             }
                //
                //             let start_rvline =
                //                 lines.rvline_of_line(text_prov, range.start);
                //
                //             // TODO: this wouldn't need to produce vlines if screen lines didn't
                //             // require them.
                //             let iter = lines
                //                 .iter_rvlines_init(
                //                     text_prov,
                //                     cache_rev,
                //                     config_id,
                //                     start_rvline,
                //                     false,
                //                 )
                //                 .take_while(|vline_info| {
                //                     vline_info.rvline.line < range.end
                //                 })
                //                 .enumerate();
                //             for (i, rvline_info) in iter {
                //                 let rvline = rvline_info.rvline;
                //                 if initial_y_idx + i < min_vline.0 {
                //                     continue;
                //                 }
                //
                //                 rvlines.push(rvline);
                //                 let vline_y = (initial_y_idx + i) * line_height;
                //                 info.insert(
                //                     rvline,
                //                     LineInfo {
                //                         y: line_y(rvline_info, vline_y) as f64 - y0,
                //                         vline_y: vline_y as f64 - y0,
                //                         vline_info: rvline_info,
                //                     },
                //                 );
                //
                //                 if initial_y_idx + i > max_vline.0 {
                //                     break;
                //                 }
                //             }
                //
                //             if is_right {
                //                 if let Some(DiffLines::Left(r)) = last_change {
                //                     // TODO: count vline count in the other editor since this is skipping an amount dependent on those vlines
                //                     let len = r.len() - r.len().min(range.len());
                //                     if len > 0 {
                //                         diff_sections.push(DiffSection {
                //                             y_idx,
                //                             height: len,
                //                             kind: DiffSectionKind::NoCode,
                //                         });
                //                         y_idx += len;
                //                     }
                //                 };
                //             }
                //         }
                //         (_, DiffLines::Both(bothinfo)) => {
                //             let start = if is_right {
                //                 bothinfo.right.start
                //             } else {
                //                 bothinfo.left.start
                //             };
                //             let len = bothinfo.right.len();
                //             let diff_height = len
                //                 - bothinfo
                //                     .skip
                //                     .as_ref()
                //                     .map(|skip| skip.len().saturating_sub(1))
                //                     .unwrap_or(0);
                //             if y_idx + diff_height < min_vline.get() {
                //                 y_idx += diff_height;
                //                 last_change = Some(change);
                //                 continue;
                //             }
                //
                //             let start_rvline = lines.rvline_of_line(text_prov, start);
                //
                //             let mut iter = lines
                //                 .iter_rvlines_init(
                //                     text_prov,
                //                     cache_rev,
                //                     config_id,
                //                     start_rvline,
                //                     false,
                //                 )
                //                 .take_while(|info| info.rvline.line < start + len);
                //             while let Some(rvline_info) = iter.next() {
                //                 let line = rvline_info.rvline.line;
                //
                //                 // Skip over the lines
                //                 if let Some(skip) = bothinfo.skip.as_ref() {
                //                     if Some(skip.start) == line.checked_sub(start) {
                //                         y_idx += 1;
                //                         // Skip by `skip` count
                //                         for _ in 0..skip.len().saturating_sub(1) {
                //                             iter.next();
                //                         }
                //                         continue;
                //                     }
                //                 }
                //
                //                 // Add the vline if it is within view
                //                 if y_idx >= min_vline.get() {
                //                     rvlines.push(rvline_info.rvline);
                //                     let vline_y = y_idx * line_height;
                //                     info.insert(
                //                         rvline_info.rvline,
                //                         LineInfo {
                //                             y: line_y(rvline_info, vline_y) as f64 - y0,
                //                             vline_y: vline_y as f64 - y0,
                //                             vline_info: rvline_info,
                //                         },
                //                     );
                //                 }
                //
                //                 y_idx += 1;
                //
                //                 if y_idx - 1 > max_vline.get() {
                //                     break;
                //                 }
                //             }
                //         }
                //     }
                //     last_change = Some(change);
                // }
                // ScreenLines {
                //     lines: Rc::new(rvlines),
                //     info: Rc::new(info),
                //     diff_sections: Some(Rc::new(diff_sections)),
                //     base,
                // }
            }
        }
    }
}

fn preedit_phantom(
    preedit: &PreeditData,
    buffer: &Buffer,
    under_line: Option<Color>,
    line: usize,
) -> Option<PhantomText> {
    let preedit = preedit.preedit.get_untracked()?;

    let (ime_line, col) = buffer.offset_to_line_col(preedit.offset);

    if line != ime_line {
        return None;
    }

    Some(PhantomText {
        kind: PhantomTextKind::Ime,
        line,
        text: preedit.text,
        affinity: None,
        final_col: col,
        merge_col: col,
        font_size: None,
        fg: None,
        bg: None,
        under_line,
        col,
    })
}

fn push_strip_suffix(line_content_original: &str, rs: &mut String) {
    if let Some(s) = line_content_original.strip_suffix("\r\n") {
        rs.push_str(s);
        rs.push_str("  ");
        // format!("{s}  ")
    } else if let Some(s) = line_content_original.strip_suffix('\n') {
        rs.push_str(s);
        rs.push(' ');
    } else {
        rs.push_str(line_content_original);
    }
}
