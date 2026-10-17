mod blame;
pub use blame::BlameView;
mod common;
mod views;

mod detail;
mod help;
pub use help::HelpView;
mod list;
mod local_changes;

pub use common::*;
pub use list::handle_search_event;
pub use views::*;
