//! Soul Sever is a terminal Soulseek client.
//!
//! An empty username opens the sign-in form and does not open a socket.
//! [`session`] dials only the host in [`config::Config`].

pub mod app;
pub mod cli;
pub mod config;
pub mod library;
pub mod model;
pub mod panels;
mod playback;
mod preview;
pub mod protocol;
pub mod quality;
mod secrets;
pub mod session;
pub mod settings;
pub mod shares;
pub mod ui;

pub use app::{App, ScrollId, Target, View};
pub use panels::{Edge, Split};
