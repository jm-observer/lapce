#![allow(unused_imports)]
use crate::window_tab::WindowTabData;
use floem::reactive::SignalGet;
use itertools::Itertools;
use tracing::{debug, info};

pub fn log(window: &WindowTabData) {
    print_screen_lines(window);
}

pub fn print_screen_lines(window: &WindowTabData) {
    for (_, editor) in &window.main_split.editors.0.get_untracked() {
        let screen_lines = editor.editor.screen_lines.get_untracked();
        for line in screen_lines.lines.as_ref() {
            info!("line={line:?}");
        }
        for (line, info) in screen_lines
            .info
            .as_ref()
            .iter()
            .sorted_by(|x, y| x.0.line.cmp(&y.0.line))
        {
            info!("line={line:?} info={info:?}");
        }
    }
}
