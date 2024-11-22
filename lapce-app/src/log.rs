#![allow(unused_imports)]
use crate::window_tab::WindowTabData;
use floem::reactive::SignalGet;
use itertools::Itertools;
use tracing::{debug, error, info};

pub fn log(window: &WindowTabData) {
    print_screen_lines(window);
}

pub fn print_screen_lines(window: &WindowTabData) {
    for (id, editor) in &window.main_split.editors.0.get_untracked() {
        let doc = editor.doc();
        if doc.content.get_untracked().is_file() {
            let screen_lines = editor.editor.screen_lines.get_untracked();
            error!("{id:?} {:?}", screen_lines.base.get_untracked(),);
            for (index, visual_line) in screen_lines.visual_lines.iter().enumerate()
            {
                error!("{index} {:?}", visual_line.visual_line);
            }
            info!("");
        }
    }
}
