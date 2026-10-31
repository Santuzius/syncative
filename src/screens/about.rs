//! About dialog: version, project links, licence and credits, and the open source licenses of the packages Syncative is built from (`assets/licenses.json`, from `scripts/update-licenses.sh`, packed by build.rs).

use std::sync::OnceLock;

use iced::{
    widget::{button, column, container, row, scrollable, svg, text_editor, Column, Space},
    Alignment, Element, Length,
};
use serde::Deserialize;

use crate::{
    branding,
    screens::add_remote::PRIVACY_POLICY,
    theme::{self, CAPTION, ROW_SPACING, TEXT, TITLE},
    widgets::{bullet, text},
};

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const ISSUES: &str = concat!(env!("CARGO_PKG_REPOSITORY"), "/issues");
const LICENSE: &str = "https://www.gnu.org/licenses/gpl-3.0.html";

/// Which part of About shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Main,
    Licenses,
    /// One of [`licenses`] by its index, with the package whose license text shows: its wording (variant) and its place in it.
    License { license: usize, variant: usize, package: usize },
}

impl Page {
    /// The page "Back" leads to.
    pub fn back(self) -> Option<Page> {
        match self {
            Page::Main => None,
            Page::Licenses => Some(Page::Main),
            Page::License { .. } => Some(Page::Licenses),
        }
    }

    /// The license pages keep their title and buttons in place and scroll in between; the main page scrolls as a whole.
    pub fn is_framed(self) -> bool {
        self != Page::Main
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Open(&'static str),
    Show(Page),
    /// Scrolling and selecting in the license text; edits are dropped.
    TextAction(text_editor::Action),
    Close,
}

/// A license with its packages, grouped by the wording of their license files; the most common wording comes first.
#[derive(Deserialize)]
struct License {
    license: String,
    variants: Vec<Variant>,
}

/// Packages whose license files have the same wording.
#[derive(Deserialize)]
struct Variant {
    text: String,
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    version: String,
    /// The copyright lines of its license file.
    notices: Vec<String>,
}

impl Package {
    fn label(&self) -> String {
        format!("{} {}", self.name, self.version).trim().to_owned()
    }
}

/// Unpacked and parsed on first use (build.rs packs them); the most used license comes first.
fn licenses() -> &'static [License] {
    static LICENSES: OnceLock<Vec<License>> = OnceLock::new();
    LICENSES.get_or_init(|| {
        let packed = include_bytes!(concat!(env!("OUT_DIR"), "/licenses.json.deflate"));
        miniz_oxide::inflate::decompress_to_vec(packed).ok().and_then(|json| serde_json::from_slice(&json).ok()).unwrap_or_default()
    })
}

/// The license text a [`Page::License`] shows: the package's copyright lines, then the text of its wording.
pub fn license_text(page: Page) -> String {
    let Page::License { license, variant, package } = page else { return String::new() };
    let Some(variant) = licenses().get(license).and_then(|l| l.variants.get(variant)) else { return String::new() };
    let notices = variant.packages.get(package).map(|p| p.notices.join("\n")).unwrap_or_default();
    if notices.is_empty() { variant.text.clone() } else { format!("{notices}\n\n{}", variant.text) }
}

/// `text`: the license text of a [`Page::License`], see [`license_text`].
pub fn view<'a>(page: Page, text: &'a text_editor::Content) -> Element<'a, Msg> {
    let dialog = container(match page {
        Page::Main => main(),
        Page::Licenses => license_list(),
        Page::License { license, variant, package } => match licenses().get(license) {
            Some(entry) => license_page(license, entry, (variant, package), text),
            None => license_list(),
        },
    })
    .padding(22)
    .max_width(520)
    .style(theme::dialog);
    if page.is_framed() { dialog.max_height(760).height(Length::Fill).into() } else { dialog.into() }
}

/// Title above, the part that scrolls, and Back and Close below.
fn framed<'a>(title: &'a str, intro: Option<String>, body: Element<'a, Msg>, back: Page) -> Element<'a, Msg> {
    let mut head = column![text(title).size(TITLE)].spacing(6);
    if let Some(intro) = intro {
        head = head.push(text(intro).size(TEXT));
    }
    column![
        head,
        body,
        row![
            button(text("Back").size(TEXT)).padding([6, 16]).style(theme::button_secondary).on_press(Msg::Show(back)),
            Space::new().width(Length::Fill),
            button(text("Close").size(TEXT)).padding([6, 16]).style(theme::button_secondary).on_press(Msg::Close),
        ]
        .align_y(Alignment::Center),
    ]
    .spacing(14)
    .into()
}

/// Scrolls `content` within the room the dialog leaves, with the slim scrollbar beside it.
fn scrolling<'a>(content: impl Into<Element<'a, Msg>>) -> Element<'a, Msg> {
    scrollable(container(content).padding(iced::Padding::default().right(12.0)))
        .height(Length::Fill)
        .direction(theme::slim_scrollbar())
        .style(theme::scrollbar)
        .into()
}

/// Every license with the packages under it; each opens its text.
fn license_list<'a>() -> Element<'a, Msg> {
    let mut list = Column::new().spacing(6);
    for (index, license) in licenses().iter().enumerate() {
        let count = license.variants.iter().map(|v| v.packages.len()).sum::<usize>();
        let mut names: Vec<&str> = license.variants.iter().flat_map(|v| &v.packages).map(|p| p.name.as_str()).collect();
        names.sort_unstable_by_key(|name| name.to_lowercase());
        names.dedup();
        let shown = names.iter().take(3).copied().collect::<Vec<_>>().join(", ");
        let summary = match count {
            1 => shown,
            n if names.len() <= 3 => format!("{n} packages: {shown}"),
            n => format!("{n} packages: {shown} and more"),
        };
        list = list.push(
            button(column![text(license.license.as_str()).size(TEXT), text(summary).size(CAPTION).style(theme::muted)].spacing(2))
                .width(Length::Fill)
                .padding([8, 12])
                .style(theme::button_secondary)
                .on_press(Msg::Show(Page::License { license: index, variant: 0, package: 0 })),
        );
    }
    framed("Open source licenses", Some("Syncative is built from these packages, each under its own license.".to_owned()), scrolling(list), Page::Main)
}

/// One license: the text for the chosen package above, the packages under this license below.
fn license_page<'a>(index: usize, license: &'static License, selected: (usize, usize), license_text: &'a text_editor::Content) -> Element<'a, Msg> {
    // All wordings in one list, by name.
    let mut all: Vec<(usize, usize, &Package)> = license.variants.iter().enumerate().flat_map(|(v, variant)| variant.packages.iter().enumerate().map(move |(p, package)| (v, p, package))).collect();
    all.sort_by_cached_key(|(_, _, package)| (package.name.to_lowercase(), package.version.clone()));
    let mut packages = Column::new().spacing(2);
    for (v, p, package) in all {
        let mut entry = column![text(package.label()).size(CAPTION)];
        if let Some(notice) = package.notices.first() {
            entry = entry.push(text(notice.as_str()).size(CAPTION).style(theme::muted));
        }
        packages = packages.push(
            button(entry)
                .width(Length::Fill)
                .padding([4, 8])
                .style(theme::button_toggle(selected == (v, p)))
                .on_press(Msg::Show(Page::License { license: index, variant: v, package: p })),
        );
    }
    let count = license.variants.iter().map(|v| v.packages.len()).sum::<usize>();
    let shown = license.variants.get(selected.0).and_then(|v| v.packages.get(selected.1)).map(Package::label).unwrap_or_default();
    let intro = if count > 1 { format!("The license of {shown}; choose another package below.") } else { format!("The license of {shown}.") };
    // Half the room for the text, half for the packages.
    let body = column![
        text_editor(license_text).height(Length::Fill).padding(8).size(CAPTION).on_action(Msg::TextAction),
        column![text(format!("Used by ({count})")).size(TEXT), scrolling(packages)].spacing(10).height(Length::Fill),
    ]
    .spacing(14);
    framed(license.license.as_str(), Some(intro), body.into(), Page::Licenses)
}

fn main<'a>() -> Element<'a, Msg> {
    let link = |label: &'a str, url: &'static str| button(text(label).size(CAPTION)).padding([5, 12]).style(theme::button_secondary).on_press(Msg::Open(url));

    let mut credits = column![
        text("Credits").size(TEXT),
        bullet("Hunter Wittenborn (hwittenborn): idea and original code of Celeste"),
        bullet("Proton AG: go-proton-api"),
        bullet("Chun-Hung Tseng: Proton-API-Bridge"),
        bullet("rclone, iced, Tabler Icons, Material Design Icons and Ionicons"),
    ]
    .spacing(4);
    // The emoji font iced_android bundles for Android 15+; its licence asks for this credit.
    if cfg!(target_os = "android") {
        credits = credits.push(bullet("Emoji on Android 15+: Twemoji by Twitter, CC-BY 4.0"));
    }

    let copyright = column![
        text("© 2022–2024 Hunter Wittenborn").size(CAPTION).style(theme::muted),
        text("© 2026 Alexander Menzel").size(CAPTION).style(theme::muted),
        text("Licensed under the GNU GPL, version 3 or later.").size(CAPTION).style(theme::muted),
        row![link("License", LICENSE), button(text("Open source licenses").size(CAPTION)).padding([5, 12]).style(theme::button_secondary).on_press(Msg::Show(Page::Licenses))]
            .spacing(ROW_SPACING)
            .padding(iced::Padding::default().top(6.0)),
    ]
    .spacing(2);

    column![
        row![
            svg(branding::logo()).width(Length::Fixed(56.0)).height(Length::Fixed(56.0)),
            column![
                text("Syncative").size(TITLE),
                text(concat!("Version ", env!("CARGO_PKG_VERSION"))).size(CAPTION).style(theme::muted),
            ]
            .spacing(2),
        ]
        .spacing(14)
        .align_y(Alignment::Center),
        text(format!("Two-way file sync between folders on {} and Google Drive or Proton Drive.", crate::util::THIS_DEVICE)).size(TEXT),
        row![link("Source", REPOSITORY), link("Privacy", PRIVACY_POLICY), link("Report issue", ISSUES)].spacing(ROW_SPACING),
        credits,
        copyright,
        row![
            Space::new().width(Length::Fill),
            button(text("Close").size(TEXT)).padding([6, 16]).style(theme::button_secondary).on_press(Msg::Close),
        ],
    ]
    .spacing(14)
    .into()
}
