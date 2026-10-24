pub(crate) mod about;
pub(crate) mod adaptive;
pub(crate) mod album_grid;
pub(crate) mod ambient;
pub(crate) mod cards;
pub(crate) mod cells;
pub(crate) mod confirm;
pub(crate) mod device_code;
pub(crate) mod equalizer;
pub(crate) mod hero;
pub(crate) mod local;
pub(crate) mod menus;
pub(crate) mod nav;
pub(crate) mod page;
pub(crate) mod picks;
pub(crate) mod pins;
pub(crate) mod playlist_editor;
pub(crate) mod popups;
pub(crate) mod shelves;
pub(crate) mod steps;
pub(crate) mod tag_editor;
pub(crate) mod text;
pub(crate) mod track_card;
pub(crate) mod tracks;
pub(crate) mod transport;
pub(crate) mod trouble;
pub(crate) mod visualizer;

pub(crate) use ui::effects;

/// The brand colours of the providers' marks.
const DEEZER: u32 = 0xa238ff;
const SUBSONIC: u32 = 0xffa31a;

pub(crate) fn provider_logo(slug: &str) -> &'static str {
    match slug {
        "subsonic" => "icons/subsonic.svg",
        "deezer" => "icons/deezer.svg",
        "local" => "icons/file-music.svg",
        "tidal" => "icons/tidal.svg",
        _ => "icons/music.svg",
    }
}

/// A small pill naming the provider an item belongs to, drawn where the shelf it sits on
/// is not obvious: a hit in a merged search, the track in the player bar.
pub(crate) fn provider_chip(slug: &str, name: &str, cx: &gpui::App) -> gpui::AnyElement {
    use gpui::prelude::*;
    use ui::ActiveTheme as _;

    let theme = *cx.theme();
    gpui::div()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .py_0p5()
        .rounded_xs()
        .bg(theme.muted)
        .child(
            gpui::svg()
                .path(icons::path(provider_logo(slug)))
                .size_3()
                .text_color(theme.muted_foreground),
        )
        .child(
            gpui::div()
                .text_size(theme.text(ui::Text::Tiny))
                .text_color(theme.muted_foreground)
                .child(name.to_owned()),
        )
        .into_any_element()
}

/// The brand colour of the provider `slug`'s mark.
pub(crate) fn provider_brand(slug: &str, cx: &gpui::App) -> gpui::Hsla {
    match slug {
        "deezer" => gpui::rgb(DEEZER).into(),
        "subsonic" => gpui::rgb(SUBSONIC).into(),
        _ => ui::ActiveTheme::theme(cx).foreground,
    }
}
