use crate::editor::lines::VisualLine;
use floem::kurbo::Rect;
use floem::reactive::{RwSignal, Scope, SignalGet, SignalUpdate};
use floem::views::editor::view::{DiffSection, LineInfo};
use floem::views::editor::visual_line::{RVLine, VLine, VLineInfo};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::rc::Rc;

// TODO(minor): We have diff sections in screen lines because Lapce uses them, but
// we don't really have support for diffs in floem-editor! Is there a better design for this?
// Possibly we should just move that out to a separate field on Lapce's editor.
// 不允许滚到到窗口没有文本！！！因此lines等不会为空
#[derive(Clone)]
pub struct ScreenLines {
    pub lines: Vec<RVLine>,
    pub visual_lines: Vec<VisualLineInfo>,
    /// Guaranteed to have an entry for each `VLine` in `lines`
    /// You should likely use accessor functions rather than this directly.
    pub info: Rc<HashMap<RVLine, LineInfo>>,
    pub diff_sections: Option<Rc<Vec<DiffSection>>>,
    // The base y position that all the y positions inside `info` are relative to.
    // This exists so that if a text layout is created outside of the view, we don't have to
    // completely recompute the screen lines (or do somewhat intricate things to update them)
    // we simply have to update the `base_y`.
    pub base: Rect,
}

#[derive(Clone, Debug)]
pub struct VisualLineInfo {
    /// 该视觉行所属折叠行（原始行）在窗口的y偏移（不是整个文档的y偏移）。若该折叠行（原始行）只有1行视觉行，则y=vline_y
    pub y: f64,
    /// 视觉行在窗口的y偏移（不是整个文档的y偏移）。
    pub vline_y: f64,
    pub visual_line: VisualLine,
}

impl ScreenLines {
    pub fn new(_cx: Scope, viewport: Rect) -> ScreenLines {
        ScreenLines {
            lines: Default::default(),
            visual_lines: Default::default(),
            info: Default::default(),
            diff_sections: Default::default(),
            base: viewport,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn clear(&mut self, viewport: Rect) {
        self.lines = Default::default();
        self.info = Default::default();
        self.diff_sections = Default::default();
        self.base = viewport;
    }

    /// Get the line info for the given rvline.
    pub fn info(&self, rvline: RVLine) -> Option<LineInfo> {
        let info = self.info.get(&rvline)?;
        // let base = self.base.get();

        Some(info.clone().with_base(self.base))
    }

    pub fn vline_info(&self, rvline: RVLine) -> Option<VLineInfo<VLine>> {
        self.info.get(&rvline).map(|info| info.vline_info)
    }

    pub fn rvline_range(&self) -> Option<(RVLine, RVLine)> {
        self.lines.first().copied().zip(self.lines.last().copied())
    }

    /// Iterate over the line info, copying them with the full y positions.
    pub fn iter_line_info(&self) -> impl Iterator<Item = LineInfo> + '_ {
        self.lines.iter().map(|rvline| self.info(*rvline).unwrap())
    }

    /// Iterate over the line info within the range, copying them with the full y positions.
    /// If the values are out of range, it is clamped to the valid lines within.
    pub fn iter_line_info_r(
        &self,
        r: RangeInclusive<RVLine>,
    ) -> impl Iterator<Item = LineInfo> + '_ {
        // We search for the start/end indices due to not having a good way to iterate over
        // successive rvlines without the view.
        // This should be good enough due to lines being small.
        let start_idx = self.lines.binary_search(r.start()).ok().or_else(|| {
            if self.lines.first().map(|l| r.start() < l).unwrap_or(false) {
                Some(0)
            } else {
                // The start is past the start of our lines
                None
            }
        });

        let end_idx = self.lines.binary_search(r.end()).ok().or_else(|| {
            if self.lines.last().map(|l| r.end() > l).unwrap_or(false) {
                Some(self.lines.len() - 1)
            } else {
                // The end is before the end of our lines but not available
                None
            }
        });

        if let (Some(start_idx), Some(end_idx)) = (start_idx, end_idx) {
            self.lines.get(start_idx..=end_idx)
        } else {
            // Hacky method to get an empty iterator of the same type
            self.lines.get(0..0)
        }
        .into_iter()
        .flatten()
        .copied()
        .map(|rvline| self.info(rvline).unwrap())
    }

    // pub fn iter_vline_info(&self) -> impl Iterator<Item = VLineInfo<()>> + '_ {
    //     self.lines
    //         .iter()
    //         .map(|vline| &self.info[vline].vline_info)
    //         .copied()
    // }

    // pub fn iter_vline_info_r(
    //     &self,
    //     r: RangeInclusive<RVLine>,
    // ) -> impl Iterator<Item = VLineInfo<()>> + '_ {
    //     // TODO(minor): this should probably skip tracking?
    //     self.iter_line_info_r(r).map(|x| x.vline_info)
    // }

    // /// Iter the real lines underlying the visual lines on the screen
    // pub fn iter_lines(&self) -> impl Iterator<Item = usize> + '_ {
    //     // We can just assume that the lines stored are contiguous and thus just get the first
    //     // buffer line and then the last buffer line.
    //     let start_vline = self.lines.first().copied().unwrap_or_default();
    //     let end_vline = self.lines.last().copied().unwrap_or_default();
    //
    //     let start_line = self.info(start_vline).unwrap().vline_info.rvline.line;
    //     let end_line = self.info(end_vline).unwrap().vline_info.rvline.line;
    //
    //     start_line..=end_line
    // }

    /// 视觉行
    // pub fn iter_visual_lines_y(
    //     &self,
    //     show_relative: bool,
    //     current_line: usize,
    // ) -> impl Iterator<Item = (String, f64)> + '_ {
    //     self.visual_lines.iter().map(move |vline| {
    //         let text = vline.visual_line.line_number(show_relative, current_line);
    //         // let info = self.info(*vline).unwrap();
    //         // let line = info.vline_info.origin_line;
    //         // if last_line == Some(line) {
    //         //     // We've already considered this line.
    //         //     return None;
    //         // }
    //         // last_line = Some(line);
    //         (text, vline.y)
    //     })
    // }

    /// Iterate over the real lines underlying the visual lines on the screen with the y position
    /// of their layout.
    /// (line, y)
    /// 应该为视觉行
    pub fn iter_lines_y(&self) -> impl Iterator<Item = (usize, f64)> + '_ {
        let mut last_line = None;
        self.lines.iter().filter_map(move |vline| {
            let info = self.info(*vline).unwrap();

            let line = info.vline_info.origin_line;

            if last_line == Some(line) {
                // We've already considered this line.
                return None;
            }

            last_line = Some(line);

            Some((line, info.y))
        })
    }

    pub fn iter_line_info_y(&self) -> impl Iterator<Item = LineInfo> + '_ {
        self.lines
            .iter()
            .map(move |vline| self.info(*vline).unwrap())
    }

    /// Get the earliest line info for a given line.
    pub fn info_for_line(&self, line: usize) -> Option<LineInfo> {
        self.info(self.first_rvline_for_line(line)?)
    }

    /// 获取原始行的视觉行信息。为none则说明被折叠，或者没有在窗口范围
    pub fn visual_line_info_for_origin_line(
        &self,
        origin_line: usize,
    ) -> Option<VisualLineInfo> {
        for visual_line in &self.visual_lines {
            match origin_line.cmp(&visual_line.visual_line.origin_line) {
                Ordering::Less => {
                    return None;
                }
                Ordering::Equal => {
                    return Some(visual_line.clone());
                }
                _ => {}
            }
        }
        None
    }

    /// Get the earliest rvline for the given line
    pub fn first_rvline_for_line(&self, line: usize) -> Option<RVLine> {
        self.lines
            .iter()
            .find(|rvline| rvline.line == line)
            .copied()
    }

    /// Get the latest rvline for the given line
    pub fn last_rvline_for_line(&self, line: usize) -> Option<RVLine> {
        self.lines
            .iter()
            .rfind(|rvline| rvline.line == line)
            .copied()
    }

    // /// Ran on [LayoutEvent::CreatedLayout](super::visual_line::LayoutEvent::CreatedLayout) to update  [`ScreenLinesBase`] &
    // /// the viewport if necessary.
    // ///
    // /// Returns `true` if [`ScreenLines`] needs to be completely updated in response
    // pub fn on_created_layout(&self, ed: &Editor, line: usize) -> bool {
    //     // The default creation is empty, force an update if we're ever like this since it should
    //     // not happen.
    //     if self.is_empty() {
    //         return true;
    //     }
    //
    //     let base = self.base.get_untracked();
    //     let vp = ed.viewport.get_untracked();
    //
    //     let is_before = self
    //         .iter_vline_info()
    //         .next()
    //         .map(|l| line < l.rvline.line)
    //         .unwrap_or(false);
    //
    //     // If the line is created before the current screenlines, we can simply shift the
    //     // base and viewport forward by the number of extra wrapped lines,
    //     // without needing to recompute the screen lines.
    //     if is_before {
    //         // TODO: don't assume line height is constant
    //         let line_height = f64::from(ed.line_height(0));
    //
    //         // We could use `try_text_layout` here, but I believe this guards against a rare
    //         // crash (though it is hard to verify) wherein the style id has changed and so the
    //         // layouts get cleared.
    //         // However, the original trigger of the layout event was when a layout was created
    //         // and it expects it to still exist. So we create it just in case, though we of course
    //         // don't trigger another layout event.
    //         let layout = ed.text_layout_trigger(line, false);
    //
    //         // One line was already accounted for by treating it as an unwrapped line.
    //         let new_lines = layout.line_count() - 1;
    //
    //         let new_y0 = base.active_viewport.y0 + new_lines as f64 * line_height;
    //         let new_y1 = new_y0 + vp.height();
    //         let new_viewport = Rect::new(vp.x0, new_y0, vp.x1, new_y1);
    //
    //         batch(|| {
    //             self.base.set(ScreenLinesBase {
    //                 active_viewport: new_viewport,
    //             });
    //             ed.viewport.set(new_viewport);
    //         });
    //
    //         // Ensure that it is created even after the base/viewport signals have been updated.
    //         // (We need the `text_layout` to still have the layout)
    //         // But we have to trigger an event still if it is created because it *would* alter the
    //         // screenlines.
    //         // TODO: this has some risk for infinite looping if we're unlucky.
    //         let _layout = ed.text_layout_trigger(line, true);
    //
    //         return false;
    //     }
    //
    //     let is_after = self
    //         .iter_vline_info()
    //         .last()
    //         .map(|l| line > l.rvline.line)
    //         .unwrap_or(false);
    //
    //     // If the line created was after the current view, we don't need to update the screenlines
    //     // at all, since the new line is not visible and has no effect on y positions
    //     if is_after {
    //         return false;
    //     }
    //
    //     // If the line is created within the current screenlines, we need to update the
    //     // screenlines to account for the new line.
    //     // That is handled by the caller.
    //     true
    // }

    pub fn log(&self) {
        tracing::info!("{:?}", self.lines);
        tracing::info!("{:?}", self.info);
    }
}
