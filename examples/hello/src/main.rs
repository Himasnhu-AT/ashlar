//! The smallest ashlar app: an empty document, a heading and a checkbox.
//!
//! `cargo xtask bench` builds it in release and reports its stripped size as
//! `hello_stripped_bytes`, the baseline every app pays for the framework.

use ashlar::prelude::*;

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct Doc {}

fn main() -> Result<(), ashlar::app::Error> {
    let mut on = false;
    App::<Doc>::new("hello", env!("CARGO_PKG_VERSION"))
        .view(move |ui, _cx: &mut Cx<'_, Doc>| {
            ui.heading("Hello, ashlar");
            ui.checkbox(&mut on, "Switch");
        })
        .run()
}
