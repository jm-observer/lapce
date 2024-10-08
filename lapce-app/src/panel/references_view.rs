use std::rc::Rc;

use floem::{reactive::SignalGet, views::Decorators, View};

use crate::panel::position::PanelContainerPosition;
use crate::{
    panel::implementation_view::common_reference_panel, window_tab::WindowTabData,
};

pub fn references_panel(
    window_tab_data: Rc<WindowTabData>,
    _position: PanelContainerPosition,
) -> impl View {
    common_reference_panel(window_tab_data.clone(), _position, move || {
        window_tab_data.main_split.references.get()
    })
    .debug_name("references panel")
}
