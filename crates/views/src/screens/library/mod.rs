mod albums;
mod artists;
mod playlists;

use std::rc::Rc;

use crate::chrome::tools::{self, Sliders};
use crate::chrome::{Chrome, Searchable, Toolbar, Tooled};
use crate::shared::confirm::{Confirm, Kind};
use crate::shared::menus::{CardMenu, Item, ItemMenu, new_playlist_menu};
use crate::shared::playlist_editor::{Edit, PlaylistEditor};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, MouseButton, Pixels, Point, Render, ScrollHandle,
    SharedString, WeakEntity, Window, div, point, px, relative,
};
use i18n::t;
use music::{Shape, Track};
use router::{Destination, LibraryTab, navigate};
use state::{
    Addition, AppSettings, Genres, Library, LibraryPart, LibraryState, Mix, Origin, Playback,
    Scan, Shelf, Sonora,
};
use ui::{
    ActiveTheme as _, Button, Card, Deck, FilterChange, LEADING, Listing, Mode, Pinnable, Popovers,
    Popup, Scrollbar, Scroller, Sort, SortAxis, TableDelegate, TableEvent, TableSource, TableState,
    Text, Toggle, Vacancy, Viewport, heading, quantize, runtime, scrolled, snapped, table,
};

use crate::shared::album_grid::{AlbumGrid, CardGrid};
use crate::shared::hero::{HeroMetaStrip, HeroPlayButton, PageHero};
use crate::shared::pins::Pinned as _;
use crate::shared::shelves;
use crate::shared::tracks::{
    self, LIBRARY_COLUMNS, PlaybackStatus, TrackField, TrackSource, Tracks, playback_status,
};
use crate::shared::{cards, cells, local, page, trouble};
use albums::{AlbumField, AlbumSource};
use artists::{ArtistField, ArtistSource};
use playlists::{PlaylistField, PlaylistSource};

impl From<LibraryTab> for Section {
    fn from(tab: LibraryTab) -> Self {
        match tab {
            LibraryTab::Songs => Section::Songs,
            LibraryTab::Favorites => Section::Favorites,
            LibraryTab::Albums => Section::Albums,
            LibraryTab::Playlists => Section::Playlists,
            LibraryTab::Artists => Section::Artists,
            LibraryTab::Mixes => Section::Mixes,
            LibraryTab::Genres => Section::Genres,
        }
    }
}

/// One page of a shelf. Songs lists the favorites on a `Shape::Saved` shelf and every song on a
/// `Shape::Catalog` one, and Favorites always lists the shelf's starred songs; the other
/// sections follow the same rule for their kind, or are made from the shelf itself: Mixes are
/// clusters of its artists scored off its tracks, and Genres are its tags or its provider's
/// categories.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Songs,
    Favorites,
    Albums,
    Playlists,
    Artists,
    Mixes,
    Genres,
}

const PINNED: [&str; 3] = ["cover", "title", "name"];
const RECENT: Sort = Sort::Descending;

#[derive(Clone)]
enum LibraryMenu {
    Background,
    Track(Box<Track>),
}

#[derive(Clone)]
enum DeckRow {
    Heading(usize),
    Cards(Vec<(usize, usize)>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeckKey {
    Heading(usize),
    Card(usize),
}

impl Section {
    const ALL: [Self; 7] = [
        Self::Songs,
        Self::Favorites,
        Self::Albums,
        Self::Playlists,
        Self::Artists,
        Self::Mixes,
        Self::Genres,
    ];

    /// The settings key a section's layout is stored under. The names predate the shelves and
    /// stay so stored layouts survive.
    fn key(self, shelf: Shelf) -> &'static str {
        match (shelf, self) {
            (Shelf::Streaming, Section::Songs) => "songs",
            (Shelf::Streaming, Section::Favorites) => "favorites",
            (Shelf::Streaming, Section::Albums) => "albums",
            (Shelf::Streaming, Section::Playlists) => "playlists",
            (Shelf::Streaming, Section::Artists) => "artists",
            (Shelf::Streaming, Section::Mixes) => "mixes",
            (Shelf::Streaming, Section::Genres) => "genres",
            (Shelf::Local, Section::Songs) => "local-songs",
            (Shelf::Local, Section::Favorites) => "local-favorites",
            (Shelf::Local, Section::Albums) => "local-albums",
            (Shelf::Local, Section::Playlists) => "local-playlists",
            (Shelf::Local, Section::Artists) => "local-artists",
            (Shelf::Local, Section::Mixes) => "local-mixes",
            (Shelf::Local, Section::Genres) => "local-genres",
        }
    }

    fn mode(self) -> Mode {
        match self {
            Section::Songs | Section::Favorites => Mode::List,
            _ => Mode::Grid,
        }
    }

    fn slot(self) -> usize {
        match self {
            Section::Songs => 0,
            Section::Favorites => 1,
            Section::Albums => 2,
            Section::Playlists => 3,
            Section::Artists => 4,
            Section::Mixes => 5,
            Section::Genres => 6,
        }
    }

    /// Whether the section is a songs listing, which always shows as a table under its hero.
    fn listing(self) -> bool {
        matches!(self, Section::Songs | Section::Favorites)
    }

    /// Whether the section has a table behind it. Mixes and Genres draw cards and genre
    /// plates instead, so the table-bound tools never see them.
    fn tabled(self) -> bool {
        !matches!(self, Section::Mixes | Section::Genres)
    }

    fn vacancy(self, shelf: Shelf, shape: Shape) -> &'static str {
        match (shelf, shape, self) {
            (Shelf::Local, _, Section::Songs) => "library-no-local-songs",
            (Shelf::Local, _, Section::Albums) => "library-no-local-albums",
            (Shelf::Local, _, Section::Playlists) => "library-no-local-playlists",
            (Shelf::Local, _, Section::Artists) => "library-no-local-artists",
            (Shelf::Streaming, Shape::Saved, Section::Songs) => "library-no-songs",
            (Shelf::Streaming, Shape::Saved, Section::Albums) => "library-no-albums",
            (Shelf::Streaming, Shape::Saved, Section::Artists) => "library-no-artists",
            (Shelf::Streaming, Shape::Catalog, Section::Songs) => "library-no-catalog-songs",
            (Shelf::Streaming, Shape::Catalog, Section::Albums) => "library-no-catalog-albums",
            (Shelf::Streaming, Shape::Catalog, Section::Artists) => "library-no-catalog-artists",
            (Shelf::Streaming, _, Section::Playlists) => "library-no-playlists",
            (_, _, Section::Favorites) => "library-no-favorites",
            (_, _, Section::Mixes) => "library-no-mixes",
            (_, _, Section::Genres) => "library-no-genres",
        }
    }

    fn glyph(self, shape: Shape) -> &'static str {
        match (self, shape) {
            (Section::Songs, Shape::Saved) => "icons/heart.svg",
            (Section::Songs, Shape::Catalog) => "icons/music.svg",
            (Section::Favorites, _) => "icons/heart-filled.svg",
            (Section::Albums, _) => "icons/disc-3.svg",
            (Section::Playlists, _) => "icons/list-music.svg",
            (Section::Artists, _) => "icons/user-round.svg",
            (Section::Mixes, _) => "icons/shuffle.svg",
            (Section::Genres, _) => "icons/music-2.svg",
        }
    }

    fn part(self) -> LibraryPart {
        match self {
            Section::Albums => LibraryPart::Albums,
            Section::Playlists => LibraryPart::Playlists,
            Section::Artists => LibraryPart::Artists,
            // Mixes are scored off the tracks and Genres is not a library part at all; both
            // are early the same span the tracks are.
            _ => LibraryPart::Tracks,
        }
    }
}

fn origin(shelf: Shelf) -> Origin {
    match shelf {
        Shelf::Streaming => Origin::saved(),
        Shelf::Local => Origin::local(),
    }
}

struct ShelfTracks {
    library: Entity<Library>,
    shelf: Shelf,
}

impl Tracks for ShelfTracks {
    fn tracks<'a>(&self, cx: &'a App) -> &'a [Track] {
        self.library.read(cx).state(self.shelf).tracks()
    }

    fn is_loading(&self, cx: &App) -> bool {
        loading(&self.library, self.shelf, Section::Songs, cx)
    }
}

/// The shelf's starred songs, the same rows the Songs page shows on a `Saved` shelf and the
/// ones beside the catalog on a `Catalog` one.
struct ShelfFavorites {
    library: Entity<Library>,
    shelf: Shelf,
}

impl Tracks for ShelfFavorites {
    fn tracks<'a>(&self, cx: &'a App) -> &'a [Track] {
        self.library.read(cx).favorite_tracks(self.shelf)
    }

    fn is_loading(&self, cx: &App) -> bool {
        self.library.read(cx).favorites_loading(self.shelf)
    }
}

fn loading(library: &Entity<Library>, shelf: Shelf, section: Section, cx: &App) -> bool {
    match section {
        // The favorites of a catalog shelf land on a fetch of their own, beside the parts.
        Section::Favorites => library.read(cx).favorites_loading(shelf),
        _ => library.read(cx).loading(shelf, section.part()),
    }
}

pub struct LibraryView {
    shelf: Shelf,
    library: Entity<Library>,
    settings: Entity<AppSettings>,
    playback: Entity<Playback>,
    playback_status: PlaybackStatus,
    section: Section,
    views: [Mode; 7],
    width: Pixels,
    card_columns: usize,
    card_tile: Pixels,
    card_heading: Pixels,
    card_rows: Rc<[DeckRow]>,
    cards_dirty: bool,
    card_scrollbar: Entity<Scrollbar>,
    scrollbar: Entity<Scrollbar>,
    songs: Entity<TableState<TrackSource>>,
    favorites: Entity<TableState<TrackSource>>,
    albums: Entity<TableState<AlbumSource>>,
    playlists: Entity<TableState<PlaylistSource>>,
    artists: Entity<TableState<ArtistSource>>,
    genres: Entity<Genres>,
    mixes: Rc<Vec<Mix>>,
    query: String,
    context_menu: Option<(LibraryMenu, Point<Pixels>)>,
    toolbar: Entity<Toolbar>,
    popovers: Popovers,
    sliders: [Sliders; 7],
    me: WeakEntity<Self>,
}

impl LibraryView {
    pub fn new(
        shelf: Shelf,
        library: Entity<Library>,
        playback: Entity<Playback>,
        genres: Entity<Genres>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let width = cells::content_width(window, Pixels::ZERO, cx);
        let settings = Sonora::global(cx).settings.clone();
        let stored = |section: Section, cx: &App| {
            let settings = settings.read(cx);
            (
                settings.table(section.key(shelf)),
                settings.sorting(section.key(shelf)),
            )
        };
        let viewed = |section: Section, cx: &App| {
            settings
                .read(cx)
                .view_or(section.key(shelf), section.mode())
        };
        let views = Section::ALL.map(|section| viewed(section, cx));

        let id = cx.entity_id();
        let scrollbar = cx.new(|_| Scrollbar::new(ScrollHandle::new()).watching(id));
        let scroll = scrollbar.read(cx).scroll().clone();

        let songs = cx.new(|cx| {
            let playlist_scrollbar = cx.new(|_| Scrollbar::inset().watching(id));
            let from = origin(shelf);
            let source = TrackSource::new(
                LIBRARY_COLUMNS,
                ShelfTracks {
                    library: library.clone(),
                    shelf,
                },
                playback.clone(),
                playlist_scrollbar,
                cx,
            )
            .from(move |_| Some(from.clone()))
            .with_liked(library.clone())
            .starrable(shelf)
            .table(cx.weak_entity());
            let mut delegate =
                TableDelegate::new(source, width, cx).with_sort(TrackField::AddedAt, RECENT, cx);
            let (layout, sorting) = stored(Section::Songs, cx);
            delegate.set_layout(layout, cx);
            if let Some(sorting) = sorting {
                delegate.set_sorting(sorting, cx);
            }
            TableState::new(delegate, cx).follow(scroll.clone())
        });
        let favorites = cx.new(|cx| {
            let playlist_scrollbar = cx.new(|_| Scrollbar::inset().watching(id));
            let from = origin(shelf);
            let source = TrackSource::new(
                LIBRARY_COLUMNS,
                ShelfFavorites {
                    library: library.clone(),
                    shelf,
                },
                playback.clone(),
                playlist_scrollbar,
                cx,
            )
            .from(move |_| Some(from.clone()))
            .with_liked(library.clone())
            .starrable(shelf)
            .table(cx.weak_entity());
            let mut delegate =
                TableDelegate::new(source, width, cx).with_sort(TrackField::AddedAt, RECENT, cx);
            let (layout, sorting) = stored(Section::Favorites, cx);
            delegate.set_layout(layout, cx);
            if let Some(sorting) = sorting {
                delegate.set_sorting(sorting, cx);
            }
            TableState::new(delegate, cx).follow(scroll.clone())
        });
        let albums = cx.new(|cx| {
            let playlist_scrollbar = cx.new(|_| Scrollbar::inset().watching(id));
            let menu = ItemMenu::new(playlist_scrollbar, cx);
            let source = AlbumSource::shelved(library.clone(), playback.clone(), menu, shelf);
            let mut delegate =
                TableDelegate::new(source, width, cx).with_sort(AlbumField::AddedAt, RECENT, cx);
            let (layout, sorting) = stored(Section::Albums, cx);
            delegate.set_layout(layout, cx);
            if let Some(sorting) = sorting {
                delegate.set_sorting(sorting, cx);
            }
            TableState::new(delegate, cx).follow(scroll.clone())
        });
        let playlists = cx.new(|cx| {
            let source = PlaylistSource::shelved(library.clone(), playback.clone(), shelf);
            let mut delegate = TableDelegate::new(source, width, cx).with_sort(
                PlaylistField::Modified,
                RECENT,
                cx,
            );
            let (layout, sorting) = stored(Section::Playlists, cx);
            delegate.set_layout(layout, cx);
            if let Some(sorting) = sorting {
                delegate.set_sorting(sorting, cx);
            }
            TableState::new(delegate, cx).follow(scroll.clone())
        });
        let artists = cx.new(|cx| {
            let source = ArtistSource::shelved(library.clone(), playback.clone(), shelf);
            let mut delegate =
                TableDelegate::new(source, width, cx).with_sort(ArtistField::AddedAt, RECENT, cx);
            let (layout, sorting) = stored(Section::Artists, cx);
            delegate.set_layout(layout, cx);
            if let Some(sorting) = sorting {
                delegate.set_sorting(sorting, cx);
            }
            TableState::new(delegate, cx).follow(scroll)
        });

        cx.observe(&library, |this, _, cx| {
            this.rebuild(cx);
            this.restore(cx);
            cx.notify();
        })
        .detach();

        cx.observe(&genres, |_, _, cx| cx.notify()).detach();
        genres.update(cx, |genres, cx| genres.load(cx));

        let chrome = Chrome::entity(cx);
        cx.observe(&chrome, |_, _, cx| cx.notify()).detach();
        cx.observe(&Scan::global(cx), |_, _, cx| cx.notify())
            .detach();

        let current_playback = playback_status(&playback, cx);
        cx.observe(&playback, |this, playback, cx| {
            let current = playback_status(&playback, cx);
            if this.playback_status == current {
                return;
            }
            this.playback_status = current;
            for table in this.tables() {
                table.refresh(cx);
            }
            cx.notify();
        })
        .detach();

        cx.subscribe(&songs, |this, _, event, cx| match event {
            TableEvent::DoubleClicked(display) => {
                let table = this.songs.clone();
                this.play(&table, *display, cx);
            }
            TableEvent::Activated(display) => {
                let table = this.tracks().clone();
                page::play_or_toggle(&table, &this.playback, *display, cx)
            }
            // on a saved shelf the list is the favorites, so removing a row unstars it
            TableEvent::Removed => {
                if this.shape(cx) == Shape::Saved {
                    tracks::drop_picked(&this.songs, cx);
                }
            }
            _ => this.persist(Section::Songs, cx),
        })
        .detach();

        cx.subscribe(&favorites, |this, _, event, cx| match event {
            TableEvent::DoubleClicked(display) => {
                let table = this.favorites.clone();
                this.play(&table, *display, cx);
            }
            TableEvent::Activated(display) => {
                let table = this.favorites.clone();
                page::play_or_toggle(&table, &this.playback, *display, cx)
            }
            // The list is the favorites on either shape, so a removed row unstars on both.
            TableEvent::Removed => this.drop_favorites(cx),
            _ => this.persist(Section::Favorites, cx),
        })
        .detach();

        cx.subscribe(&albums, |this, _, event, cx| match event {
            TableEvent::DoubleClicked(display) | TableEvent::Activated(display) => {
                this.open_album(*display, cx)
            }
            TableEvent::Removed => this.drop_albums(cx),
            _ => {
                this.cards_dirty = true;
                this.persist(Section::Albums, cx);
            }
        })
        .detach();

        cx.subscribe(&playlists, |this, _, event, cx| match event {
            TableEvent::DoubleClicked(display) | TableEvent::Activated(display) => {
                this.open_playlist(*display, cx)
            }
            TableEvent::Removed => this.drop_playlists(cx),
            _ => {
                this.cards_dirty = true;
                this.persist(Section::Playlists, cx);
            }
        })
        .detach();

        cx.subscribe(&artists, |this, _, event, cx| match event {
            TableEvent::DoubleClicked(display) | TableEvent::Activated(display) => {
                this.open_artist(*display, cx)
            }
            TableEvent::Removed => this.drop_artists(cx),
            _ => {
                this.cards_dirty = true;
                this.persist(Section::Artists, cx);
            }
        })
        .detach();

        let me = cx.entity();
        let toolbar = Toolbar::searchable(&me, cx);

        let card_scrollbar = cx.new(|_| Scrollbar::new(ScrollHandle::new()).watching(id));
        let mixes: Rc<Vec<Mix>> = library.read(cx).mixes(shelf, cx);

        let mut view = Self {
            shelf,
            library,
            settings,
            playback,
            playback_status: current_playback,
            section: Section::Songs,
            views,
            width,
            card_columns: 0,
            card_tile: Pixels::ZERO,
            card_heading: Pixels::ZERO,
            card_rows: Rc::from([]),
            cards_dirty: true,
            card_scrollbar,
            scrollbar,
            songs,
            favorites,
            albums,
            playlists,
            artists,
            genres,
            mixes,
            query: String::new(),
            context_menu: None,
            toolbar,
            popovers: Popovers::default(),
            sliders: Section::ALL.map(|_| Sliders::default()),
            me: me.downgrade(),
        };
        view.restore(cx);
        view
    }

    fn create_playlist(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.context_menu = None;
        PlaylistEditor::open(
            Edit::Create {
                addition: Addition::Tracks(Vec::new()),
                shelf: self.shelf,
            },
            window,
            cx,
        );
        cx.notify();
    }

    pub fn section(&self) -> Section {
        self.section
    }

    /// The table behind a section, `None` for the two that draw cards and plates instead.
    fn table(&self, section: Section) -> Option<&dyn ui::Listing> {
        match section {
            Section::Songs => Some(&self.songs),
            Section::Favorites => Some(&self.favorites),
            Section::Albums => Some(&self.albums),
            Section::Playlists => Some(&self.playlists),
            Section::Artists => Some(&self.artists),
            Section::Mixes | Section::Genres => None,
        }
    }

    fn tables(&self) -> [&dyn ui::Listing; 5] {
        [
            &self.songs,
            &self.favorites,
            &self.albums,
            &self.playlists,
            &self.artists,
        ]
    }

    fn tracks(&self) -> &Entity<TableState<TrackSource>> {
        &self.songs
    }

    /// The songs table a listing section shows.
    fn track_table(&self, section: Section) -> &Entity<TableState<TrackSource>> {
        match section {
            Section::Favorites => &self.favorites,
            _ => &self.songs,
        }
    }

    /// How many rows or cards the current section has to show, counting a filtered genre or
    /// mix grid the way a table counts its kept rows.
    fn rows(&self, cx: &App) -> usize {
        match self.section {
            Section::Mixes => self.mix_list().len(),
            Section::Genres => self.genre_list(cx).len(),
            section => self.table(section).map_or(0, |table| table.row_count(cx)),
        }
    }

    fn shape(&self, cx: &App) -> Shape {
        self.library.read(cx).shape(self.shelf)
    }

    fn column_toggles(&self, cx: &App) -> Vec<Toggle> {
        self.table(self.section)
            .map(|table| {
                table
                    .toggles(cx)
                    .into_iter()
                    .filter(|toggle| !PINNED.contains(&toggle.key))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn switch_column(&mut self, key: &str, cx: &mut Context<Self>) {
        if PINNED.contains(&key) {
            return;
        }
        let Some(table) = self.table(self.section) else {
            return;
        };

        let mut layout = table.layout(cx);
        layout.toggle(key);
        table.set_layout(layout, cx);
        self.persist(self.section, cx);
        cx.notify();
    }

    fn persist(&mut self, section: Section, cx: &mut Context<Self>) {
        let Some(table) = self.table(section) else {
            return;
        };
        let key = section.key(self.shelf);
        page::store(&self.settings.clone(), table, key, key, cx);
    }

    /// Fills filter axes the storage names but the tables have not narrowed yet. This runs at
    /// construction and whenever the library reloads, so a range saved before its rows arrived
    /// still lands once the bounds are known.
    fn restore(&mut self, cx: &mut Context<Self>) {
        for section in Section::ALL {
            let Some(table) = self.table(section) else {
                continue;
            };
            let key = section.key(self.shelf);
            page::restore(&self.settings.clone(), table, key, cx);
        }
    }

    /// Whether the shelf has no folder to list. A scan in flight is not that: a folder is only
    /// recorded once its scan lands, so the setup screen would otherwise cover the whole of the
    /// first import, which is the longest one there is.
    fn unconfigured(&self, cx: &App) -> bool {
        self.shelf.local()
            && Sonora::global(cx).session.read(cx).local_paths().is_empty()
            && Scan::global(cx).read(cx).progress().is_none()
    }

    /// What an empty local page says while a scan is filling it. The page is not empty, it is
    /// early, so it counts the files read instead of offering the vacancy's caption.
    fn scanning(&self, cx: &App) -> Option<Vacancy> {
        if !self.shelf.local() || self.rows(cx) > 0 {
            return None;
        }
        let progress = Scan::global(cx).read(cx).progress()?;
        let caption = match progress.found {
            0 => t!("library-scanning"),
            found => t!(
                "library-scanning-progress",
                read = progress.read,
                found = found
            ),
        };
        let shape = self.library.read(cx).shape(self.shelf);
        Some(Vacancy::new(caption).icon(self.section.glyph(shape)))
    }

    /// Whether the current section's content is narrowed by the toolbar's filter: the table's
    /// own sieve on a tabled section, the query against the card names on the others.
    fn filtering(&self, cx: &App) -> bool {
        match self.table(self.section) {
            Some(table) => table.filtering(cx),
            None => !self.query.trim().is_empty(),
        }
    }

    fn note(&self, cx: &App) -> Option<Vacancy> {
        if let Some(scanning) = self.scanning(cx) {
            return Some(scanning);
        }
        if self.section == Section::Genres {
            return self.genres_note(cx);
        }
        if loading(&self.library, self.shelf, self.section, cx) {
            return None;
        }
        let library = self.library.read(cx);
        match library.state(self.shelf) {
            LibraryState::Loading => return None,
            LibraryState::Failed(reason) => {
                return Some(self.lost("library-lost", t!("library-not-loaded"), reason));
            }
            _ if self.rows(cx) > 0 => return None,
            _ => {}
        }

        let problem = library.part_problem(self.shelf, self.section.part());
        let shape = library.shape(self.shelf);

        Some(match (self.filtering(cx), problem) {
            (true, _) => Vacancy::new(t!("library-no-matches")),
            (false, Some(reason)) => {
                self.lost("library-part-lost", t!("library-part-not-loaded"), reason)
            }
            (false, None) => {
                Vacancy::new(i18n::lookup(self.section.vacancy(self.shelf, shape), None))
                    .icon(self.section.glyph(shape))
            }
        })
    }

    /// What the Genres page says when it has nothing to show. The genre list loads and fails
    /// beside the library's own parts, so its vacancy reasons from the entity's state rather
    /// than the shelf's.
    fn genres_note(&self, cx: &App) -> Option<Vacancy> {
        let genres = self.genres.read(cx);
        if genres.is_loading() {
            return None;
        }
        if let Some(reason) = genres.error() {
            let genres = self.genres.clone();
            let reason = reason.to_owned();
            return Some(trouble::lost(
                "library-genres-lost",
                t!("library-not-loaded"),
                Some(reason.as_str()),
                move |_, _, cx| {
                    genres.update(cx, |genres, cx| genres.reload(cx));
                },
            ));
        }
        if !self.genre_list(cx).is_empty() {
            return None;
        }
        let shape = self.library.read(cx).shape(self.shelf);
        Some(match self.query.trim().is_empty() {
            true => Vacancy::new(t!("library-no-genres")).icon(Section::Genres.glyph(shape)),
            false => Vacancy::new(t!("library-no-matches")),
        })
    }

    /// The shelf's genres, narrowed to what the toolbar's filter names when it is set.
    fn genre_list(&self, cx: &App) -> Rc<Vec<music::Genre>> {
        let genres = self.genres.read(cx).genres();
        let query = self.query.trim().to_lowercase();
        match query.is_empty() {
            true => genres,
            false => Rc::new(
                genres
                    .iter()
                    .filter(|genre| genre.name.to_lowercase().contains(&query))
                    .cloned()
                    .collect(),
            ),
        }
    }

    /// The shelf's mixes, narrowed to what the toolbar's filter names when it is set.
    fn mix_list(&self) -> Rc<Vec<Mix>> {
        let query = self.query.trim().to_lowercase();
        match query.is_empty() {
            true => self.mixes.clone(),
            false => Rc::new(
                self.mixes
                    .iter()
                    .filter(|mix| mix.name.to_lowercase().contains(&query))
                    .cloned()
                    .collect(),
            ),
        }
    }

    /// The state a shelf that did not load shows, with a button that loads it again.
    fn lost(&self, id: &'static str, label: SharedString, reason: &str) -> Vacancy {
        let library = self.library.clone();
        let shelf = self.shelf;

        trouble::lost(id, label, Some(reason), move |_, _, cx| {
            library.update(cx, |library, cx| library.refresh(shelf, cx));
        })
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let shelf = self.shelf;
        self.library
            .update(cx, |library, cx| library.refresh(shelf, cx));
    }

    pub fn select(&mut self, section: Section, cx: &mut Context<Self>) {
        if self.section != section {
            self.scrollbar
                .read(cx)
                .scroll()
                .set_offset(Point::default());
            self.cards_dirty = true;
        }
        self.section = section;
        if self.mode() == Mode::List
            && let Some(table) = self.table(section)
        {
            table.set_width(self.width, cx);
        }
        cx.notify();
    }

    fn viewport(scroll: &ScrollHandle, window: &Window) -> Viewport {
        quantize(scroll, window);
        let visible = scroll.bounds().size.height;

        Viewport::measured(scrolled(scroll), visible, window)
    }

    fn header(&self, cx: &Context<Self>) -> AnyElement {
        let table = self.track_table(self.section);
        let state = table.read(cx);
        let delegate = state.delegate();
        let listed = delegate.row_count();
        // While the songs are still arriving, the provider's own total is the count to show,
        // so it does not climb a page at a time. A filter counts what it kept, and the
        // favorites, which arrive without a provider total, always count what is listed.
        let count = match table.filtering(cx) || self.section == Section::Favorites {
            true => listed,
            false => self
                .library
                .read(cx)
                .expected(self.shelf, self.section.part())
                .map_or(listed, |expected| expected.max(listed)),
        };
        let duration: std::time::Duration = (0..listed)
            .filter_map(|display| delegate.source().peek(delegate.row(display), cx))
            .map(|track| track.duration)
            .sum();
        let mut strip = HeroMetaStrip::new().text(t!("count-songs", count = count));
        if !duration.is_zero() {
            strip = strip.text(runtime(duration));
        }
        let eyebrow = match self.shelf {
            Shelf::Local => t!("nav-local"),
            Shelf::Streaming => t!("nav-library"),
        };
        let (title, icon, eyebrow) = match (self.section, self.shape(cx), self.shelf) {
            (Section::Favorites, _, _) => (t!("nav-favorites"), "icons/heart-filled.svg", eyebrow),
            (_, Shape::Catalog, _) | (_, _, Shelf::Local) => {
                (t!("nav-songs"), "icons/disc-3.svg", eyebrow)
            }
            (_, Shape::Saved, _) => (
                t!("library-liked-songs"),
                "icons/heart-filled.svg",
                t!("detail-playlist"),
            ),
        };

        PageHero::new("library-hero", title)
            .fallback(icon)
            .accent()
            .eyebrow(eyebrow)
            .meta(strip)
            .actions(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(HeroPlayButton::listed(
                        "play-library",
                        t!("library-play-liked-songs"),
                        table,
                        self.playback.clone(),
                    ))
                    .child(HeroPlayButton::shuffle_listed(
                        "shuffle-library",
                        table,
                        self.playback.clone(),
                    )),
            )
            .into_any_element()
    }

    fn play(
        &mut self,
        table: &Entity<TableState<TrackSource>>,
        display: usize,
        cx: &mut Context<Self>,
    ) {
        let queued = tracks::ordered(table, cx);
        let from = tracks::whence(table, cx);
        self.playback
            .update(cx, |playback, cx| playback.start(queued, display, from, cx));
    }

    fn open_album(&mut self, display: usize, cx: &mut Context<Self>) {
        let album = {
            let state = self.albums.read(cx);
            let row = state.delegate().row(display);
            state.delegate().source().at(row, cx)
        };
        let Some(album) = album else {
            return;
        };
        navigate(Destination::Album(album.id.into()), cx);
    }

    fn open_playlist(&mut self, display: usize, cx: &mut Context<Self>) {
        let playlist = {
            let state = self.playlists.read(cx);
            let row = state.delegate().row(display);
            state.delegate().source().at(row, cx)
        };
        let Some(playlist) = playlist else {
            return;
        };
        navigate(Destination::Playlist(playlist.id.into()), cx);
    }

    fn open_artist(&mut self, display: usize, cx: &mut Context<Self>) {
        let artist = {
            let state = self.artists.read(cx);
            let row = state.delegate().row(display);
            state.delegate().source().at(row, cx)
        };
        let Some(artist) = artist else {
            return;
        };
        navigate(Destination::Artist(artist.id.into()), cx);
    }

    /// Unstars the picked favorites. On a catalog shelf that only unstars them, so it asks
    /// nothing; on a saved one it is removing them from the library, so it does.
    fn drop_favorites(&mut self, cx: &mut Context<Self>) {
        let rows = self.favorites.read(cx).delegate().picked();
        let tracks: Vec<_> = {
            let state = self.favorites.read(cx);
            let source = state.delegate().source();
            rows.iter().filter_map(|&row| source.at(row, cx)).collect()
        };
        let Some(first) = tracks.first().and_then(|track| track.id.clone()) else {
            return;
        };
        let library = self.library.clone();
        let table = self.favorites.clone();
        let count = tracks.len();
        let unstarring = Confirm::unstarring(&first, cx);
        let apply = move |cx: &mut App| {
            library.update(cx, |library, cx| {
                library.save_tracks(tracks, false, cx);
            });
            table.update(cx, |table, cx| {
                table.delegate_mut().clear_selection();
                cx.notify();
            });
        };
        match unstarring {
            true => apply(cx),
            false => Confirm::ask(Kind::LibrarySongs(count), apply, cx),
        }
    }

    fn drop_albums(&mut self, cx: &mut Context<Self>) {
        let rows = self.albums.read(cx).delegate().picked();
        let albums: Vec<_> = {
            let state = self.albums.read(cx);
            let source = state.delegate().source();
            rows.iter().filter_map(|&row| source.at(row, cx)).collect()
        };
        if albums.is_empty() {
            return;
        }
        let library = self.library.clone();
        let table = self.albums.clone();
        let count = albums.len();
        let starred = Confirm::unstarring(&albums[0].id, cx);
        let apply = move |cx: &mut App| {
            library.update(cx, |library, cx| {
                for album in albums {
                    library.toggle_album(album, cx);
                }
            });
            table.update(cx, |table, cx| {
                table.delegate_mut().clear_selection();
                cx.notify();
            });
        };
        match starred {
            true => apply(cx),
            false => Confirm::ask(Kind::Albums(count), apply, cx),
        }
    }

    fn drop_artists(&mut self, cx: &mut Context<Self>) {
        // Nothing to unfollow where following is not a thing the provider has.
        if !Sonora::global(cx)
            .session
            .read(cx)
            .capabilities_of(self.shelf)
            .follow_artists
        {
            return;
        }
        let rows = self.artists.read(cx).delegate().picked();
        let artists: Vec<_> = {
            let state = self.artists.read(cx);
            let source = state.delegate().source();
            rows.iter().filter_map(|&row| source.at(row, cx)).collect()
        };
        if artists.is_empty() {
            return;
        }
        let library = self.library.clone();
        let table = self.artists.clone();
        let count = artists.len();
        let starred = Confirm::unstarring(&artists[0].id, cx);
        let apply = move |cx: &mut App| {
            library.update(cx, |library, cx| {
                for artist in artists {
                    library.toggle_artist(artist, cx);
                }
            });
            table.update(cx, |table, cx| {
                table.delegate_mut().clear_selection();
                cx.notify();
            });
        };
        match starred {
            true => apply(cx),
            false => Confirm::ask(Kind::Artists(count), apply, cx),
        }
    }

    fn drop_playlists(&mut self, cx: &mut Context<Self>) {
        let rows = self.playlists.read(cx).delegate().picked();
        let ids: Vec<String> = {
            let state = self.playlists.read(cx);
            let source = state.delegate().source();
            rows.iter()
                .filter_map(|&row| source.at(row, cx))
                .filter(|playlist| !playlist.owned)
                .map(|playlist| playlist.id)
                .collect()
        };
        if ids.is_empty() {
            return;
        }
        let library = self.library.clone();
        let table = self.playlists.clone();
        Confirm::ask(
            Kind::Playlists(ids.len()),
            move |cx| {
                library.update(cx, |library, cx| {
                    for id in ids {
                        library.remove_playlist_from_library(id, cx);
                    }
                });
                table.update(cx, |table, cx| {
                    table.delegate_mut().clear_selection();
                    cx.notify();
                });
            },
            cx,
        );
    }

    fn resize(&mut self, window: &Window, cx: &mut Context<Self>) {
        let width = cells::content_width(window, Pixels::ZERO, cx);
        if (width - self.width).abs() < px(0.5) {
            return;
        }
        self.width = width;

        if self.mode() == Mode::List
            && let Some(table) = self.table(self.section)
        {
            table.set_width(width, cx);
        }
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.cards_dirty = true;
        self.mixes = self.library.read(cx).mixes(self.shelf, cx);
        for table in self.tables() {
            table.rebuild(cx);
        }
    }

    fn cards(&mut self, window: &Window, cx: &App) -> AnyElement {
        let theme = *cx.theme();
        let inset = theme.metrics.inset;
        let room = cells::content_width(window, page::reserved(inset), cx);
        let layout = CardGrid::layout(room);
        let columns = layout.columns;
        let card = layout.card;

        let scroll = self.card_scrollbar.read(cx).scroll().clone();
        let gap = deck_gap(window);
        let tile = Card::tile_height(card, window, cx);
        let heading = head_height(window, cx);
        let depth = (scrolled(&scroll) - inset).max(Pixels::ZERO);

        let repacked = self.card_columns != columns
            || (self.card_tile - tile).abs() >= px(0.5)
            || (self.card_heading - heading).abs() >= px(0.5);
        let anchor = (repacked && self.card_columns != 0 && !self.cards_dirty)
            .then(|| {
                let heights = deck_heights(&self.card_rows, self.card_tile, self.card_heading);
                let (index, offset) = Deck::at(&heights, gap, depth);
                let share = match heights.get(index) {
                    Some(height) if *height > Pixels::ZERO => offset / *height,
                    _ => 0.,
                };
                deck_key(&self.card_rows, index).map(|key| (key, share))
            })
            .flatten();

        if self.card_columns != columns {
            self.card_columns = columns;
            self.cards_dirty = true;
        }
        if self.cards_dirty {
            self.card_rows = match self.section {
                Section::Songs | Section::Favorites => {
                    deck(self.track_table(self.section), columns, cx)
                }
                Section::Albums => deck(&self.albums, columns, cx),
                Section::Playlists => deck(&self.playlists, columns, cx),
                Section::Artists => deck(&self.artists, columns, cx),
                Section::Mixes | Section::Genres => Vec::new(),
            }
            .into();
            self.cards_dirty = false;
        }
        self.card_tile = tile;
        self.card_heading = heading;

        let heights = deck_heights(&self.card_rows, tile, heading);
        if let Some((index, share)) =
            anchor.and_then(|(key, share)| deck_row(&self.card_rows, key).map(|row| (row, share)))
        {
            let top = Deck::tops(&heights, gap)
                .get(index)
                .copied()
                .unwrap_or(Pixels::ZERO);
            let into = heights.get(index).copied().unwrap_or(Pixels::ZERO) * share;
            scroll.set_offset(point(Pixels::ZERO, -(top + into + inset)));
        }

        let rows = self.card_rows.clone();
        let section = self.section;
        let view = self.me.clone();

        Scroller::new("library-cards", &self.card_scrollbar)
            .py(inset)
            .child(
                Deck::new("library-deck")
                    .rows(heights)
                    .gap(gap)
                    .draw(move |index, _, cx| {
                        let Some(row) = rows.get(index) else {
                            return div().into_any_element();
                        };
                        let Some(view) = view.upgrade() else {
                            return div().into_any_element();
                        };
                        let view = view.read(cx);

                        match row {
                            DeckRow::Heading(display) => {
                                let label = match section {
                                    Section::Songs | Section::Favorites => view
                                        .track_table(section)
                                        .read(cx)
                                        .delegate()
                                        .group(*display, cx),
                                    Section::Albums => {
                                        view.albums.read(cx).delegate().group(*display, cx)
                                    }
                                    Section::Playlists => {
                                        view.playlists.read(cx).delegate().group(*display, cx)
                                    }
                                    Section::Artists => {
                                        view.artists.read(cx).delegate().group(*display, cx)
                                    }
                                    Section::Mixes | Section::Genres => None,
                                };

                                div()
                                    .px(inset)
                                    .children(label.map(|label| head(label, cx)))
                                    .into_any_element()
                            }
                            DeckRow::Cards(cards) => {
                                let row = match section {
                                    Section::Songs | Section::Favorites => CardGrid::new(room)
                                        .children(cards.iter().filter_map(|&(display, row)| {
                                            view.track_card(section, display, row, card, cx)
                                        }))
                                        .into_any_element(),
                                    Section::Albums => {
                                        view.album_grid(cards, room, cx).into_any_element()
                                    }
                                    Section::Playlists => CardGrid::new(room)
                                        .children(cards.iter().filter_map(|&(display, row)| {
                                            view.playlist_card(display, row, card, cx)
                                        }))
                                        .into_any_element(),
                                    Section::Artists => CardGrid::new(room)
                                        .children(cards.iter().filter_map(|&(display, row)| {
                                            view.artist_card(display, row, card, cx)
                                        }))
                                        .into_any_element(),
                                    Section::Mixes | Section::Genres => div().into_any_element(),
                                };

                                div().px(inset).child(row).into_any_element()
                            }
                        }
                    }),
            )
            .into_any_element()
    }

    fn track_card(
        &self,
        section: Section,
        display: usize,
        row: usize,
        card: Pixels,
        cx: &App,
    ) -> Option<AnyElement> {
        let theme = *cx.theme();
        let listing = self.track_table(section);
        let track = listing.read(cx).delegate().source().at(row, cx)?;
        let playable = track.playable;
        let pressed = (listing.clone(), self.playback.clone());
        let played = pressed.clone();
        let current = listing.read(cx).delegate().source().now_playing(row, cx);
        let playing = current == Some(true);
        let artists = cells::artist_links(
            SharedString::from(format!("library-track-artist-{display}")),
            track.artist_refs.clone(),
            track.artists.clone(),
            theme.muted_foreground,
        )
        .text_size(theme.text(Text::Small))
        .truncate();

        let pin = track.pin();
        let context = track.clone();
        let view = self.me.clone();

        Some(
            Card::new(("library-track", display), SharedString::from(track.name))
                .tile(card)
                .cover(track.cover)
                .when_some(pin, Pinnable::pin)
                .weight(FontWeight::SEMIBOLD)
                .flat()
                .when(track.explicit, Card::explicit)
                .bare_meta(artists)
                .menu(move |event, _, cx| {
                    let Some(view) = view.upgrade() else {
                        return;
                    };
                    view.update(cx, |this, cx| {
                        this.tracks().read(cx).delegate().source().menu().reset(cx);
                        this.context_menu = Some((
                            LibraryMenu::Track(Box::new(context.clone())),
                            event.position,
                        ));
                        cx.notify();
                    });
                })
                .when(playable, move |card| {
                    card.play(playing, move |_, _, cx| match current {
                        Some(_) => played.1.update(cx, |playback, cx| playback.toggle_play(cx)),
                        None => page::play(&played.0, &played.1, display, cx),
                    })
                    .press(move |_, _, cx| page::play(&pressed.0, &pressed.1, display, cx))
                })
                .into_any_element(),
        )
    }

    /// The Mixes page: a card per artist cluster the shelf's tracks make. There is no page
    /// behind a mix, so the card's press and its play control do the same thing.
    fn mix_grid(&self, note: Option<Vacancy>, window: &Window, cx: &App) -> AnyElement {
        let theme = *cx.theme();
        let inset = theme.metrics.inset;
        let room = cells::content_width(window, page::reserved(inset), cx);
        let layout = CardGrid::layout(room);
        let gap = deck_gap(window);

        let content =
            match note {
                Some(note) => note.size_full().into_any_element(),
                None => {
                    let mixes = self.mix_list();
                    match mixes.is_empty() {
                        true => CardGrid::new(room)
                            .children((0..layout.columns).map(|place| {
                                Card::skeleton(("library-mix-pending", place))
                                    .tile(layout.card)
                                    .into_any_element()
                            }))
                            .into_any_element(),
                        false => div()
                            .flex()
                            .flex_col()
                            .gap_y(gap)
                            .children(mixes.chunks(layout.columns).enumerate().map(
                                |(row, chunk)| {
                                    CardGrid::new(room)
                                        .children(chunk.iter().enumerate().map(|(place, mix)| {
                                            self.mix_card(
                                                row * layout.columns + place,
                                                mix,
                                                layout.card,
                                                cx,
                                            )
                                        }))
                                        .into_any_element()
                                },
                            ))
                            .into_any_element(),
                    }
                }
            };

        Scroller::new("library-page", &self.scrollbar)
            .p(inset)
            .child(content)
            .into_any_element()
    }

    /// One mix: its play control toggles the running mix in place, and pressing the card
    /// does the same since there is no page behind it.
    fn mix_card(&self, index: usize, mix: &Mix, card: Pixels, cx: &App) -> AnyElement {
        let theme = *cx.theme();
        let seed = mix.seed.clone();
        let playable = seed.playable;
        let origin = seed
            .id
            .as_deref()
            .map(Origin::radio)
            .map(|origin| origin.named(seed.name.clone()));
        let state = origin
            .as_ref()
            .and_then(|origin| self.playback.read(cx).playing_from(origin));
        let playing = state == Some(true);

        let artists = cells::artist_links(
            SharedString::from(format!("library-mix-artist-{index}")),
            seed.artist_refs.clone(),
            seed.artists.clone(),
            theme.muted_foreground,
        )
        .text_size(theme.text(Text::Small))
        .truncate();

        let playback = self.playback.clone();
        let toggled = self.playback.clone();
        let play_mix = mix.clone();
        let press_mix = mix.clone();
        let press_state = state.clone();

        Card::new(
            ("library-mix", index),
            t!("library-artist-mix", artist = mix.name.clone()),
        )
        .tile(card)
        .cover(seed.cover.clone())
        .fallback("icons/shuffle.svg")
        .weight(FontWeight::SEMIBOLD)
        .flat()
        .bare_meta(artists)
        .menu(CardMenu::opener(
            Item::Track(mix.seed.clone()),
            self.playback.clone(),
        ))
        .when(playable, |card| {
            card.play(playing, move |_, _, cx| match state {
                Some(_) => playback.update(cx, |playback, cx| playback.toggle_play(cx)),
                None => playback.update(cx, |playback, cx| playback.play_mix(&play_mix, cx)),
            })
            .press(move |_, _, cx| match press_state {
                Some(_) => toggled.update(cx, |playback, cx| playback.toggle_play(cx)),
                None => toggled.update(cx, |playback, cx| playback.play_mix(&press_mix, cx)),
            })
        })
        .into_any_element()
    }

    /// The Genres page: the shelf's genres as plates in a grid, each opening its page.
    fn genre_grid(&self, note: Option<Vacancy>, window: &Window, cx: &App) -> AnyElement {
        let theme = *cx.theme();
        let inset = theme.metrics.inset;
        let width = cells::content_width(window, page::reserved(inset), cx);
        let genres = self.genre_list(cx);
        let loading = genres.is_empty() && self.genres.read(cx).is_loading();

        Scroller::new("library-page", &self.scrollbar)
            .p(inset)
            .child(match note {
                Some(note) => note.size_full().into_any_element(),
                None if loading => shelves::grid_skeleton(width, window, cx),
                None => shelves::grid("library-genres", genres, width, window, cx),
            })
            .into_any_element()
    }

    fn album_grid(&self, cards: &[(usize, usize)], room: Pixels, cx: &App) -> AlbumGrid {
        let albums = cards.iter().filter_map(|&(display, row)| {
            self.albums
                .read(cx)
                .delegate()
                .source()
                .at(row, cx)
                .map(|album| (display, album))
        });

        AlbumGrid::new("library-album", room, albums, self.playback.clone())
    }

    fn playlist_card(
        &self,
        display: usize,
        row: usize,
        card: Pixels,
        cx: &App,
    ) -> Option<AnyElement> {
        let playlist = self.playlists.read(cx).delegate().source().at(row, cx)?;
        let build = match self.shelf.local() {
            true => cards::imported_playlist_card,
            false => cards::playlist_card,
        };

        Some(
            build(("library-playlist", display), &playlist, &self.playback, cx)
                .tile(card)
                .flat()
                .into_any_element(),
        )
    }

    fn artist_card(
        &self,
        display: usize,
        row: usize,
        card: Pixels,
        cx: &App,
    ) -> Option<AnyElement> {
        let artist = self.artists.read(cx).delegate().source().at(row, cx)?;

        Some(
            cards::artist_card(("library-artist", display), &artist, &self.playback, cx)
                .tile(card)
                .flat()
                .into_any_element(),
        )
    }
}

impl Render for LibraryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.resize(window, cx);

        let theme = *cx.theme();
        let inset = theme.metrics.inset;
        let mode = self.mode();
        if mode == Mode::List
            && let Some(table) = self.table(self.section)
        {
            table.claim(cx);
            let scroll = self.scrollbar.read(cx).scroll().clone();
            let viewport = match self.section.listing() {
                true => page::viewport(&scroll, inset, window),
                false => Self::viewport(&scroll, window),
            };
            table.set_viewport(viewport, cx);
        }

        let context_menu = self.context_menu.clone().map(|(target, position)| {
            let menu = match target {
                LibraryMenu::Track(track) => self
                    .tracks()
                    .read(cx)
                    .delegate()
                    .source()
                    .menu()
                    .for_track(&track, cx),
                LibraryMenu::Background => new_playlist_menu(cx.listener(|this, _, window, cx| {
                    this.create_playlist(window, cx);
                })),
            };
            Popup::new(position, menu).on_close(cx.listener(|this, _, _, cx| {
                this.context_menu = None;
                cx.notify();
            }))
        });
        let view = cx.entity().downgrade();
        let section = self.section;
        let note = self.note(cx);
        let content = match (self.section, mode) {
            _ if self.unconfigured(cx) => local::unconfigured("configure-local-folder")
                .size_full()
                .into_any_element(),
            (Section::Genres, _) => self.genre_grid(note, window, cx),
            (Section::Mixes, _) => self.mix_grid(note, window, cx),
            (section, Mode::List) if section.listing() => {
                Scroller::new("library-page", &self.scrollbar)
                    .pt(inset)
                    .pb(inset)
                    .child(div().px(inset).child(self.header(cx)))
                    .child(table(self.track_table(section)))
                    .when_some(note, |this, note| this.child(note))
                    .into_any_element()
            }
            (_, Mode::List) => Scroller::new("library-page", &self.scrollbar)
                .pb(inset)
                .when_some(self.table(self.section), |this, table| {
                    this.child(table.element())
                })
                .when_some(note, |this, note| this.child(note))
                .into_any_element(),
            (_, Mode::Grid) => match note {
                Some(note) => note.size_full().into_any_element(),
                None => self.cards(window, cx),
            },
        };

        div()
            .relative()
            .size_full()
            .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                if section != Section::Playlists {
                    return;
                }
                window.prevent_default();
                let Some(view) = view.upgrade() else {
                    return;
                };
                view.update(cx, |this, cx| {
                    this.context_menu = Some((LibraryMenu::Background, event.position));
                    cx.notify();
                });
            })
            .child(content)
            .when_some(context_menu, |this, menu| this.child(menu))
    }
}

impl Searchable for LibraryView {
    fn search(&mut self, query: &str, cx: &mut Context<Self>) {
        self.cards_dirty = true;
        self.query = query.to_owned();
        for table in self.tables() {
            table.set_query(query, cx);
        }
        cx.notify();
    }

    fn hint() -> SharedString {
        "filter-library".into()
    }
}

impl LibraryView {
    fn sorts(&self, cx: &App) -> Vec<SortAxis> {
        self.table(self.section)
            .map_or_else(Vec::new, |table| table.sortables(cx))
    }

    fn set_sort(&mut self, key: &'static str, cx: &mut Context<Self>) {
        if let Some(table) = self.table(self.section) {
            table.cycle_sort(key, cx);
        }
        self.cards_dirty = true;
        cx.notify();
    }

    fn mode(&self) -> Mode {
        match (self.section.listing(), self.section.tabled()) {
            (true, _) => Mode::List,
            (false, false) => Mode::Grid,
            (false, true) => self.views[self.section.slot()],
        }
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        let section = self.section;
        self.views[section.slot()] = mode;
        if mode == Mode::List
            && let Some(table) = self.table(section)
        {
            table.set_width(self.width, cx);
        }

        let settings = self.settings.clone();
        let key = section.key(self.shelf);
        settings.update(cx, |settings, cx| settings.set_view(key, mode, cx));
        cx.notify();
    }
}

impl Tooled for LibraryView {
    fn toolbar(&self) -> Entity<Toolbar> {
        self.toolbar.clone()
    }

    fn tools(&self, cx: &App) -> Vec<AnyElement> {
        // Mixes and Genres have no table, so none of the table-bound tools apply to them;
        // the toolbar's filter still narrows their cards by name.
        if !self.section.tabled() {
            return Vec::new();
        }
        let columned = self.me.clone();
        let filtered = self.me.clone();
        let sorted = self.me.clone();
        let viewed = self.me.clone();

        let columns = matches!(self.mode(), Mode::List).then(|| {
            tools::columns(&self.popovers, self.column_toggles(cx), move |key, cx| {
                columned
                    .update(cx, |view, cx| view.switch_column(key, cx))
                    .ok();
            })
        });

        let created = self.me.clone();
        let create = (self.section == Section::Playlists).then(|| {
            Button::new("new-playlist")
                .icon("icons/plus.svg")
                .tooltip("menu-new-playlist")
                .small()
                .ghost()
                .on_click(move |_, window, cx| {
                    created
                        .update(cx, |view, cx| view.create_playlist(window, cx))
                        .ok();
                })
                .into_any_element()
        });

        let mut tools = Vec::new();
        tools.extend(create);
        tools.extend(columns);
        // A section keeps its funnel while anything is narrowed, even once the axes have gone
        // with the rows, or an empty result would lock the filter that emptied it in place.
        let filters = self
            .table(self.section)
            .map_or_else(Vec::new, |table| table.filters(cx));
        let narrowed = self
            .table(self.section)
            .is_some_and(|table| table.narrowed(cx));
        if !filters.is_empty() || narrowed {
            tools.push(tools::filters(
                &self.popovers,
                &self.sliders[self.section.slot()],
                filters,
                move |change, cx| {
                    filtered.update(cx, |view, cx| view.filter(change, cx)).ok();
                },
                cx,
            ));
        }
        tools.push(tools::sorts(
            &self.popovers,
            self.sorts(cx),
            move |key, cx| {
                sorted.update(cx, |view, cx| view.set_sort(key, cx)).ok();
            },
            cx,
        ));
        let switchable = self.section.tabled() && !self.section.listing();
        tools.extend(switchable.then(|| {
            tools::views(&self.popovers, self.mode(), move |mode, cx| {
                viewed.update(cx, |view, cx| view.set_mode(mode, cx)).ok();
            })
        }));
        tools
    }
}

impl LibraryView {
    fn filter(&mut self, change: FilterChange, cx: &mut Context<Self>) {
        self.cards_dirty = true;
        let section = self.section;
        if let Some(table) = self.table(section) {
            table.filter(change, cx);
        }
        self.persist(section, cx);
        cx.notify();
    }
}

fn deck<S: TableSource>(state: &Entity<TableState<S>>, columns: usize, cx: &App) -> Vec<DeckRow> {
    let state = state.read(cx);
    let delegate = state.delegate();
    let mut rows = Vec::new();
    let mut cards = Vec::with_capacity(columns);
    let mut group: Option<SharedString> = None;

    for display in 0..delegate.row_count() {
        let label = delegate.group(display, cx);
        match &label {
            Some(text) if group.as_ref() != Some(text) => {
                if !cards.is_empty() {
                    rows.push(DeckRow::Cards(std::mem::take(&mut cards)));
                }
                rows.push(DeckRow::Heading(display));
            }
            _ => {}
        }
        group = label;
        cards.push((display, delegate.row(display)));
        if cards.len() == columns {
            rows.push(DeckRow::Cards(std::mem::take(&mut cards)));
            cards.reserve(columns);
        }
    }
    if !cards.is_empty() {
        rows.push(DeckRow::Cards(cards));
    }

    rows
}

fn deck_key(rows: &[DeckRow], item_ix: usize) -> Option<DeckKey> {
    match rows.get(item_ix)? {
        DeckRow::Heading(display) => Some(DeckKey::Heading(*display)),
        DeckRow::Cards(cards) => cards.first().map(|(display, _)| DeckKey::Card(*display)),
    }
}

fn deck_row(rows: &[DeckRow], key: DeckKey) -> Option<usize> {
    rows.iter().position(|row| match (row, key) {
        (DeckRow::Heading(display), DeckKey::Heading(anchor)) => *display == anchor,
        (DeckRow::Cards(cards), DeckKey::Card(anchor)) => {
            cards.iter().any(|(display, _)| *display == anchor)
        }
        _ => false,
    })
}

fn head(label: SharedString, cx: &App) -> AnyElement {
    heading(label, cx)
        .w_full()
        .pt_2()
        .line_height(relative(LEADING))
        .into_any_element()
}

fn head_height(window: &Window, cx: &App) -> Pixels {
    let title = px((cx.theme().text(Text::Title) / px(1.) * LEADING).round());

    snapped(window.rem_size() * 0.5 + title, window)
}

fn deck_gap(window: &Window) -> Pixels {
    snapped(window.rem_size() * 1.5, window)
}

fn deck_heights(rows: &[DeckRow], tile: Pixels, heading: Pixels) -> Vec<Pixels> {
    rows.iter()
        .map(|row| match row {
            DeckRow::Heading(_) => heading,
            DeckRow::Cards(_) => tile,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_anchor_follows_a_repacked_row() {
        let before = vec![
            DeckRow::Cards(vec![(0, 10), (1, 11)]),
            DeckRow::Cards(vec![(2, 12), (3, 13)]),
        ];
        let after = vec![
            DeckRow::Cards(vec![(0, 10)]),
            DeckRow::Cards(vec![(1, 11)]),
            DeckRow::Cards(vec![(2, 12)]),
            DeckRow::Cards(vec![(3, 13)]),
        ];

        let anchor = deck_key(&before, 1).unwrap();

        assert_eq!(anchor, DeckKey::Card(2));
        assert_eq!(deck_row(&after, anchor), Some(2));
    }

    #[test]
    fn heading_anchor_survives_a_repack() {
        let rows = vec![DeckRow::Heading(0), DeckRow::Cards(vec![(0, 10), (1, 11)])];

        let anchor = deck_key(&rows, 0).unwrap();

        assert_eq!(anchor, DeckKey::Heading(0));
        assert_eq!(deck_row(&rows, anchor), Some(0));
    }
}
