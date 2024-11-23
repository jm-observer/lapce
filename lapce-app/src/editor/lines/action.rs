use crate::editor::gutter::{FoldingDisplayItem, FoldingRange};

pub enum UpdateFolding {
    UpdateByItem(FoldingDisplayItem),
    New(Vec<FoldingRange>),
}

impl From<FoldingDisplayItem> for UpdateFolding {
    fn from(value: FoldingDisplayItem) -> Self {
        Self::UpdateByItem(value)
    }
}

impl From<Vec<FoldingRange>> for UpdateFolding {
    fn from(value: Vec<FoldingRange>) -> Self {
        Self::New(value)
    }
}
