use labrador_ui::{
    assets::asset_cache::AssetSource,
    elements::{CacheOption, ConstrainedBox, CornerRadius, Element, Image, Radius},
};

pub(crate) const LABRADOR_ICON_ASSET_PATH: &str = "bundled/png/local.png";

pub(crate) fn render_labrador_icon(size: f32, corner_radius: f32) -> Box<dyn Element> {
    ConstrainedBox::new(
        Image::new(
            AssetSource::Bundled {
                path: LABRADOR_ICON_ASSET_PATH,
            },
            CacheOption::BySize,
        )
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(corner_radius)))
        .finish(),
    )
    .with_width(size)
    .with_height(size)
    .finish()
}
