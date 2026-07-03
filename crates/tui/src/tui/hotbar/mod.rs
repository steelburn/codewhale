//! Hotbar action registry foundation and command surface coordination.
//!
//! Config, sidebar rendering, and key dispatch consume this action surface and
//! the built-in actions defined here. The command surface ties together state
//! management, binding persistence, command resolution, and status display.

pub mod actions;
pub mod command_surface;
pub mod setup;

pub use actions::HotbarActionRegistry;
#[allow(unused_imports)]
pub use command_surface::HotbarCommandSurface;
