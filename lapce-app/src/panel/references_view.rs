use std::rc::Rc;

use floem::reactive::RwSignal;
use floem::{reactive::SignalGet, views::Decorators, View};

use crate::common::{common_tab_header, Tabs};
use crate::panel::position::PanelContainerPosition;
use crate::{
    panel::implementation_view::common_reference_panel, window_tab::WindowTabData,
};

pub fn references_panel(
    window_tab_data: Rc<WindowTabData>,
    _position: PanelContainerPosition,
) -> impl View {
    let config = window_tab_data.common.config;
    common_tab_header(window_tab_data, Tabs::new(config))
    // common_reference_panel(window_tab_data.clone(), _position, move || {
    //     window_tab_data.main_split.references.get()
    // })
    // .debug_name("references panel")
}
