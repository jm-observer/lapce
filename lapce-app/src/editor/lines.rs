use floem::peniko::Color;
use floem::reactive::{RwSignal, Scope, SignalGet};
use floem::text::{
    Attrs, AttrsList, LineHeightValue, TextLayout, Wrap, FONT_SYSTEM,
};
use lapce_xi_rope::Interval;
use std::fmt::{Debug, Formatter};
use std::rc::Rc;
use std::sync::Arc;

use floem_editor_core::buffer::rope_text::RopeText;
use floem_editor_core::cursor::CursorAffinity;
use tracing::warn;

use crate::config::color::LapceColor;
use crate::config::LapceConfig;
use crate::doc::{DiagnosticData, Doc};
use crate::editor::gutter::FoldingRanges;
use floem::views::editor::layout::TextLayoutLine;
use floem::views::editor::listener::Listener;
use floem::views::editor::phantom_text::{
    PhantomText, PhantomTextKind, PhantomTextLine, PhantomTextMultiLine,
};
use floem::views::editor::text::{Document, PreeditData, Styling, WrapMethod};
use floem::views::editor::visual_line::{
    LayoutEvent, RVLine, ResolvedWrap, TextLayoutProvider, VLine, VLineInfo,
};
use floem::views::editor::{Editor, EditorStyle};
use floem_editor_core::buffer::Buffer;
use floem_editor_core::word::{get_char_property, CharClassification};
use itertools::Itertools;
use lapce_xi_rope::spans::{Spans, SpansBuilder};
use lsp_types::{DiagnosticSeverity, InlayHint, InlayHintLabel, Position};
use smallvec::SmallVec;

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
pub struct Lines {
    origin_lines: Vec<OriginLine>,
    origin_folded_lines: Vec<OriginFoldedLine>,
    visual_lines: Vec<VisualLine>,
    // pub font_sizes: Rc<EditorFontSizes>,
    // font_size_cache_id: FontSizeCacheId,
    wrap: ResolvedWrap,
    pub layout_event: Listener<LayoutEvent>,
    max_width: f64,

    // editor: Editor
    pub inlay_hints: Option<Spans<InlayHint>>,
    pub completion_pos: (usize, usize),
    pub folding_ranges: FoldingRanges,
    pub buffer: Buffer,
    pub diagnostics: DiagnosticData,
    pub completion_lens: Option<String>,

    /// Current inline completion text, if any.
    /// This will be displayed even on views that are not focused.
    pub inline_completion: Option<String>,
    /// (line, col)
    pub inline_completion_pos: (usize, usize),
    pub preedit: PreeditData,
}

impl Lines {
    pub fn new(cx: Scope) -> Self {
        Self {
            wrap: ResolvedWrap::None,
            // font_size_cache_id: id,
            layout_event: Listener::new_empty(cx),
            origin_lines: vec![],
            origin_folded_lines: vec![],
            visual_lines: vec![],
            max_width: 0.0,

            inlay_hints: None,
            completion_pos: (0, 0),
            folding_ranges: Default::default(),
            buffer: Buffer::new(""),
            diagnostics: DiagnosticData {
                expanded: cx.create_rw_signal(true),
                diagnostics: cx.create_rw_signal(im::Vector::new()),
                diagnostics_span: cx.create_rw_signal(SpansBuilder::new(0).build()),
            },
            completion_lens: None,
            inline_completion: None,
            inline_completion_pos: (0, 0),
            preedit: PreeditData::new(cx),
        }
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

    // return do_update
    pub fn update(&mut self, editor: &Editor) -> bool {
        self.clear();
        let rope_text = editor.rope_text();
        let last_line = rope_text.last_line();

        let mut current_line = 0;
        let mut origin_folded_line_index = 0;
        let mut visual_line_index = 0;
        while current_line <= last_line {
            let text_layout = editor.new_text_layout(current_line);
            let origin_line_start = text_layout.phantom_text.line;
            let origin_line_end = text_layout.phantom_text.last_line;

            let width = text_layout.text.size().width;
            if width > self.max_width {
                self.max_width = width;
            }

            for origin_line in origin_line_start..=origin_line_end {
                self.origin_lines.push(OriginLine {
                    line_index: origin_line,
                    start_offset: rope_text.offset_of_line(origin_line),
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
                    rope_text.offset_of_line_col(offset_info.0, offset_info.1);

                let offset_info = text_layout
                    .phantom_text
                    .origin_position_of_final_col(visual_offset_end);
                let origin_interval_end =
                    rope_text.offset_of_line_col(offset_info.0, offset_info.1);
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
                start: rope_text.offset_of_line(origin_line_start),
                end: rope_text.offset_of_line(origin_line_end + 1),
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
        // if self.visual_lines.len() > 2 {
        //     tracing::error!("Lines origin_lines={} origin_folded_lines={} visual_lines={}", self.origin_lines.len(), self.origin_folded_lines.len(), self.visual_lines.len());
        //     tracing::error!("{:?}", self.origin_lines);
        //     tracing::error!("{:?}", self.origin_folded_lines);
        //     tracing::error!("{:?}\n", self.visual_lines);
        // }
        warn!("update_lines done");
        true
    }

    pub fn wrap(&self) -> ResolvedWrap {
        self.wrap
    }

    /// Set the wrapping style
    ///
    /// Does nothing if the wrapping style is the same as the current one.
    /// Will trigger a clear of the text layouts if the wrapping style is different.
    pub fn set_wrap(&mut self, wrap: ResolvedWrap, editor: &Editor) {
        if wrap == self.wrap {
            return;
        }
        self.wrap = wrap;
        self.update(editor);
    }

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
    ) -> PhantomTextLine {
        let buffer = &self.buffer;
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
        let (inline_completion_line, inline_completion_col) =
            self.inline_completion_pos;
        let inline_completion_text = config
            .editor
            .enable_inline_completion
            .then_some(())
            .and(self.inline_completion.as_ref())
            .filter(|_| {
                line == inline_completion_line
                    && !folded_ranges.contain_position(Position {
                        line: inline_completion_line as u32,
                        character: inline_completion_col as u32,
                    })
            })
            .map(|completion| PhantomText {
                kind: PhantomTextKind::Completion,
                col: inline_completion_col,
                text: completion.clone(),
                affinity: Some(CursorAffinity::Backward),
                fg: Some(config.color(LapceColor::COMPLETION_LENS_FOREGROUND)),
                font_size: Some(config.editor.completion_lens_font_size()),
                // font_family: Some(config.editor.completion_lens_font_family()),
                bg: None,
                under_line: None,
                final_col: inline_completion_col,
                line,
                merge_col: inline_completion_col,
                // TODO: italics?
            });
        if let Some(inline_completion_text) = inline_completion_text {
            text.push(inline_completion_text);
        }

        // todo filter by folded?
        if let Some(preedit) = preedit_phantom(
            &self.preedit,
            &self.buffer,
            Some(config.color(LapceColor::EDITOR_FOREGROUND)),
            line,
        ) {
            text.push(preedit)
        }
        text.extend(folded_ranges.into_phantom_text(&buffer, &config, line));

        PhantomTextLine::new(line, origin_text_len, text)
    }

    fn new_text_layout(&self, doc: &Doc, mut line: usize) -> Arc<TextLayoutLine> {
        // TODO: we could share text layouts between different editor views given some knowledge of
        // their wrapping
        let style = doc.clone();
        let es = doc.editor_style().get_untracked();
        let viewport = doc.viewport().get_untracked();
        let config: Arc<LapceConfig> = doc.common.config.get_untracked();

        let text = doc.rope_text();
        line = doc.visual_line_of_line(line);

        let mut line_content = String::new();
        // Get the line content with newline characters replaced with spaces
        // and the content without the newline characters
        // TODO: cache or add some way that text layout is created to auto insert the spaces instead
        // though we immediately combine with phantom text so that's a thing.
        let line_content_original = text.line_content(line);
        let mut font_system = FONT_SYSTEM.lock();
        push_strip_suffix(&line_content_original, &mut line_content);

        let family = style.font_family(line);
        let font_size = style.font_size(line);
        let attrs = Attrs::new()
            .color(es.ed_text_color())
            .family(&family)
            .font_size(font_size as f32)
            .line_height(LineHeightValue::Px(style.line_height(line)));

        let phantom_text = self.phantom_text(&es, line, &config);
        let mut collapsed_line_col = phantom_text.folded_line();
        let multi_styles: Vec<(usize, usize, Color, Attrs)> = style
            .line_styles(line)
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
            push_strip_suffix(&text.line_content(collapsed_line), &mut line_content);

            let offset_col = phantom_text.final_text_len();
            let family = style.font_family(line);
            let font_size = style.font_size(line) as f32;
            let attrs = Attrs::new()
                .color(es.ed_text_color())
                .family(&family)
                .font_size(font_size)
                .line_height(LineHeightValue::Px(style.line_height(line)));
            // let (next_phantom_text, collapsed_line_content, styles, next_collapsed_line_col)
            //     = calcuate_line_text_and_style(collapsed_line, &next_line_content, style.clone(), edid, &es, doc.clone(), offset_col, attrs);

            let next_phantom_text = self.phantom_text(&es, collapsed_line, &config);
            collapsed_line_col = next_phantom_text.folded_line();
            let styles: Vec<(usize, usize, Color, Attrs)> = style
                .line_styles(collapsed_line)
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
        let indent_line = style.indent_line(line, &line_content_original);

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
        let offset = text.first_non_blank_character_on_line(indent_line);
        let (_, col) = text.offset_to_line_col(offset);
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
