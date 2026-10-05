//! Logical menu locations survive refreshed ordering without rebinding account actions.

use super::*;

pub(super) struct MenuLocation {
    page: MenuPage,
    anchor: Option<String>,
}

impl MenuLocation {
    pub(super) fn capture(page: MenuPage, inventory: &FrozenAccountInventory) -> Self {
        let index = match page {
            MenuPage::Quick(page)
            | MenuPage::QuickOptions(page)
            | MenuPage::Strategy(MenuOrigin::Quick(page)) => inventory
                .quick_indices()
                .get(page * QUICK_PAGE_SIZE)
                .copied(),
            MenuPage::Overview(page)
            | MenuPage::Browse(page)
            | MenuPage::Strategy(MenuOrigin::Overview(page)) => Some(page * PAGE_SIZE),
            MenuPage::Refresh(first) => Some(first),
            MenuPage::Detail(index)
            | MenuPage::Usage(index)
            | MenuPage::Actions(index)
            | MenuPage::More(index)
            | MenuPage::Membership(index)
            | MenuPage::Rename(index)
            | MenuPage::Credits(index, _) => Some(index),
            MenuPage::ChoosePage { origin, .. } => {
                let mut location = Self::capture(origin.page(), inventory);
                location.page = page;
                return location;
            }
            MenuPage::Home
            | MenuPage::Settings(_)
            | MenuPage::Primary
            | MenuPage::Confirm
            | MenuPage::Result
            | MenuPage::Login
            | MenuPage::Strategy(MenuOrigin::Settings(_)) => None,
        };
        Self {
            page,
            anchor: index
                .and_then(|index| inventory.accounts.get(index))
                .map(|account| account.id.clone()),
        }
    }

    pub(super) fn restore(&self, inventory: &FrozenAccountInventory) -> MenuPage {
        let index = self.anchor.as_ref().and_then(|id| {
            inventory
                .accounts
                .iter()
                .position(|account| &account.id == id)
        });
        let quick_page = |page: usize| {
            index
                .and_then(|index| {
                    inventory
                        .quick_indices()
                        .iter()
                        .position(|actual| *actual == index)
                })
                .map_or_else(
                    || page.min(inventory.quick_pages() - 1),
                    |position| position / QUICK_PAGE_SIZE,
                )
        };
        let list_page = |page: usize| {
            index.map_or_else(
                || page.min(inventory.pages() - 1),
                |index| index / PAGE_SIZE,
            )
        };
        let origin = |origin| match origin {
            MenuOrigin::Quick(page) => MenuOrigin::Quick(quick_page(page)),
            MenuOrigin::Overview(page) => MenuOrigin::Overview(list_page(page)),
            MenuOrigin::Settings(page) => MenuOrigin::Settings(page),
        };
        match self.page {
            MenuPage::Quick(page) => MenuPage::Quick(quick_page(page)),
            MenuPage::QuickOptions(page) => MenuPage::QuickOptions(quick_page(page)),
            MenuPage::Overview(page) => MenuPage::Overview(list_page(page)),
            MenuPage::Browse(page) => MenuPage::Browse(list_page(page)),
            MenuPage::Refresh(first) => MenuPage::Refresh(list_page(first / PAGE_SIZE) * PAGE_SIZE),
            MenuPage::Strategy(value) => MenuPage::Strategy(origin(value)),
            MenuPage::ChoosePage {
                first,
                end,
                origin: value,
            } => {
                let origin = origin(value);
                let pages = match origin {
                    MenuOrigin::Quick(_) => inventory.quick_pages(),
                    MenuOrigin::Overview(_) | MenuOrigin::Settings(_) => inventory.pages(),
                };
                let first = first.min(pages - 1);
                MenuPage::ChoosePage {
                    first,
                    end: end.min(pages).max(first + 1),
                    origin,
                }
            }
            MenuPage::Detail(_)
            | MenuPage::Usage(_)
            | MenuPage::Actions(_)
            | MenuPage::More(_)
            | MenuPage::Membership(_)
            | MenuPage::Rename(_)
            | MenuPage::Credits(_, _) => {
                let Some(index) = index else {
                    return MenuPage::Overview(0);
                };
                match self.page {
                    MenuPage::Detail(_) => MenuPage::Detail(index),
                    MenuPage::Usage(_) => MenuPage::Usage(index),
                    MenuPage::Actions(_) => MenuPage::Actions(index),
                    MenuPage::More(_) => MenuPage::More(index),
                    MenuPage::Membership(_) => MenuPage::Membership(index),
                    MenuPage::Rename(_) => MenuPage::Rename(index),
                    MenuPage::Credits(_, page) => MenuPage::Credits(index, page),
                    _ => unreachable!("captured account location"),
                }
            }
            page => page,
        }
    }
}
