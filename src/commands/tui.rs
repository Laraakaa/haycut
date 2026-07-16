//! Terminal UI entry point.
//!
//! The implementation is deliberately kept behind private layers so this
//! module remains the stable command facade.

mod app;
mod demo;
mod editor;
mod model;
mod runtime;
mod view;

pub(crate) fn run(demo: bool) -> i32 {
    app::run(demo)
}
