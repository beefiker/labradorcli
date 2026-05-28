use anyhow::Result;
use root_view::RootView;
pub mod root_view;

extern crate labrador_ui;
use labrador_ui::platform;

fn main() -> Result<()> {
    let app_builder =
        platform::AppBuilder::new(platform::AppCallbacks::default(), Box::new(()), None);
    let _ = app_builder.run(move |ctx| {
        ctx.add_window(labrador_ui::AddWindowOptions::default(), |_| RootView::new());
    });

    Ok(())
}
