//! Session-sharing logic related to the terminal view.

pub(in crate::terminal::view) mod adapter;
pub(in crate::terminal::view) mod sharer;
mod view_impl;
mod viewer;

pub(in crate::terminal::view) use {adapter::Adapter as SharedSessionAdapter, viewer::Viewer};
