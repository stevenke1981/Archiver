//! Application state machine: [`Model`] + [`Msg`] -> [`update`] with side effects behind
//! the [`Effects`] trait, so the whole UI logic runs headlessly in unit tests.

pub mod effects;
pub mod menus;
pub mod model;
pub mod msg;
pub mod update;
pub mod winrar;
pub mod worker;

pub use effects::Effects;
pub use model::*;
pub use msg::*;
pub use update::update;
