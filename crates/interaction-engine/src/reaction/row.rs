//! The row-selection primitive is intentionally not an `ActionPlan`: no
//! generic Binding, AXPress or pointer operation can represent this action.

use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};

use crate::environment::binding::RowBinding;

#[derive(Debug, Clone, PartialEq)]
pub struct RowSelectionActionPlan {
    pub binding: RowBinding,
}

impl RowSelectionActionPlan {
    pub fn new(binding: RowBinding) -> Result<Self, String> {
        if binding.source.selected != Some(false)
            || binding.source.ax_selected_settable != Some(true)
            || !binding.source.table_ax_enabled.permits_row_selection()
            || !binding.source.row_ax_enabled.permits_row_selection()
            || binding.hit_point.is_none()
            || binding.continuity.is_none()
            || binding.device_context.is_none()
        {
            return Err("row selection requires a supported unselected AX row capability".into());
        }
        Ok(Self { binding })
    }

    pub fn primitive(&self) -> Primitive {
        Primitive::AxAction {
            action: AxActionKind::SetSelectedTrue,
        }
    }
}
