//! The RatBlocker Linux filtering daemon.
//!
//! Split into a library so the daemon and its privileged helper share one
//! definition of the settings file, and so the pieces can be tested directly.

#![forbid(unsafe_code)]

pub mod config;
pub mod dbus;
pub mod dns;
pub mod state;
pub mod updater;
