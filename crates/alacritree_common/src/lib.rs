//! What every alacritree crate that runs an external program needs: a child
//! built without a console window, a pool that keeps the wait off the UI
//! thread, the WSL side of a Windows host, and where each tool lives. It
//! also holds the models that cross integrations, such as the agent status
//! and the side a pane lives on, and a digest that is the same on every run.

pub mod agent_status;
pub mod command_ext;
pub mod digest;
pub mod dll_search;
pub mod jobs;
pub mod settings;
pub mod side;
pub mod tools;
pub mod wsl;
pub mod wsl_helper;
