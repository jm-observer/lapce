#![allow(unused_imports)]
use crate::window_tab::WindowTabData;
use floem::reactive::SignalGet;
use itertools::Itertools;
use tracing::{debug, error, info};

pub fn log(window: &WindowTabData) {
    print_screen_lines(window);
}

pub fn print_screen_lines(window: &WindowTabData) {
    for (_, editor) in &window.main_split.editors.0.get_untracked() {
        let content = editor.doc().content.get_untracked();
        let path = content.path();
        if let Some(path) = path {
            info!("{:?}", path);
            editor.doc().lines.with_untracked(|x| x.log());
            info!("");
        }
    }
}
