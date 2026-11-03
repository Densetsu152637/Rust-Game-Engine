//! Scriptable Workshop world for [`FakeBackend`](crate::FakeBackend).
//!
//! Requests are answered by `ugc_poll`, one step per call: a submission walks through the
//! upload stages (one per poll) before it is applied to the catalog, and a download needs
//! `download_polls` polls. Nothing touches the disk or the network.

use std::collections::BTreeMap;

use crate::types::{SteamId, WorkshopItemId};
use crate::workshop::{
    ItemUpdate, ItemVisibility, PublicQuery, PublicSort, QueryPage, RequestId, UgcQuery, UgcRaw,
    UpdateProgress, UpdateStage, WorkshopItem,
};

const PAGE_SIZE: usize = 50;
/// Polls a submission needs before it completes.
const SUBMIT_STEPS: u32 = 5;

#[derive(Debug, Clone)]
pub(crate) struct FakeUgc {
    next_request: u64,
    next_item: u64,
    pub(crate) catalog: BTreeMap<WorkshopItemId, WorkshopItem>,
    pub(crate) updates: BTreeMap<RequestId, (WorkshopItemId, ItemUpdate, u32)>,
    pub(crate) replies: Vec<UgcRaw>,
    /// Item to `(polls left, polls total)`.
    pub(crate) downloads: BTreeMap<WorkshopItemId, (u32, u32)>,
    pub(crate) download_polls: u32,
    /// Finished downloads send no `Downloaded` reply (Steam does this for current items).
    pub(crate) silent_downloads: bool,
    pub(crate) applied: Vec<(WorkshopItemId, ItemUpdate)>,
}

impl Default for FakeUgc {
    fn default() -> Self {
        Self {
            next_request: 1,
            next_item: 1000,
            catalog: BTreeMap::new(),
            updates: BTreeMap::new(),
            replies: Vec::new(),
            downloads: BTreeMap::new(),
            download_polls: 2,
            silent_downloads: false,
            applied: Vec::new(),
        }
    }
}

impl FakeUgc {
    pub(crate) fn request(&mut self) -> RequestId {
        let id = RequestId(self.next_request);
        self.next_request += 1;
        id
    }

    pub(crate) fn create(&mut self, owner: SteamId) -> WorkshopItemId {
        let id = WorkshopItemId(self.next_item);
        self.next_item += 1;
        self.catalog.insert(
            id,
            WorkshopItem {
                id,
                title: String::new(),
                description: String::new(),
                owner,
                visibility: ItemVisibility::Private,
                tags: Vec::new(),
                time_created: 0,
                time_updated: 0,
                file_size: 0,
                votes_up: 0,
                votes_down: 0,
                subscriptions: 0,
                banned: false,
            },
        );
        id
    }

    pub(crate) fn progress(&self, request: RequestId) -> Option<UpdateProgress> {
        let (_, _, step) = self.updates.get(&request)?;
        let (stage, done, total) = match step {
            0 | 1 => (UpdateStage::PreparingConfig, 0, 0),
            2 => (UpdateStage::PreparingContent, 0, 0),
            3 => (UpdateStage::UploadingContent, 50, 100),
            _ => (UpdateStage::CommittingChanges, 0, 0),
        };
        Some(UpdateProgress { stage, done, total })
    }

    /// Advances submissions; completed ones are applied and answered.
    pub(crate) fn step_updates(&mut self) {
        let mut finished = Vec::new();
        for (request, (_, _, step)) in &mut self.updates {
            *step += 1;
            if *step >= SUBMIT_STEPS {
                finished.push(*request);
            }
        }
        for request in finished {
            let Some((item, update, _)) = self.updates.remove(&request) else {
                continue;
            };
            if let Some(entry) = self.catalog.get_mut(&item) {
                if let Some(title) = &update.title {
                    entry.title = title.clone();
                }
                if let Some(description) = &update.description {
                    entry.description = description.clone();
                }
                if let Some(visibility) = update.visibility {
                    entry.visibility = visibility;
                }
                if let Some(tags) = &update.tags {
                    entry.tags = tags.clone();
                }
                entry.time_updated += 1;
            }
            self.applied.push((item, update));
            self.replies.push(UgcRaw::Submitted {
                request,
                result: Ok((item, false)),
            });
        }
    }

    pub(crate) fn query(&self, query: &UgcQuery, me: SteamId) -> QueryPage {
        let visible = |item: &&WorkshopItem| {
            !item.banned && (item.visibility != ItemVisibility::Private || item.owner == me)
        };
        match query {
            UgcQuery::ById(ids) => {
                let items: Vec<_> = ids
                    .iter()
                    .filter_map(|id| self.catalog.get(id))
                    .filter(visible)
                    .cloned()
                    .collect();
                QueryPage {
                    total: items.len() as u32,
                    items,
                }
            }
            UgcQuery::Public(q) => self.public(q),
        }
    }

    fn public(&self, q: &PublicQuery) -> QueryPage {
        let needle = q.search.as_ref().map(|s| s.to_lowercase());
        let mut items: Vec<&WorkshopItem> = self
            .catalog
            .values()
            .filter(|i| i.visibility == ItemVisibility::Public && !i.banned)
            .filter(|i| q.required_tags.iter().all(|t| i.tags.contains(t)))
            .filter(|i| {
                needle.as_ref().is_none_or(|n| {
                    i.title.to_lowercase().contains(n) || i.description.to_lowercase().contains(n)
                })
            })
            .collect();
        match q.sort {
            PublicSort::MostVoted | PublicSort::Trending => {
                items.sort_by_key(|i| std::cmp::Reverse((i.votes_up, i.id)))
            }
            PublicSort::Newest => items.sort_by_key(|i| std::cmp::Reverse((i.time_created, i.id))),
            PublicSort::MostSubscribed => {
                items.sort_by_key(|i| std::cmp::Reverse((i.subscriptions, i.id)))
            }
            PublicSort::TextSearch => {}
        }
        let total = items.len() as u32;
        let start = (q.page.max(1) as usize - 1) * PAGE_SIZE;
        QueryPage {
            items: items
                .into_iter()
                .skip(start)
                .take(PAGE_SIZE)
                .cloned()
                .collect(),
            total,
        }
    }
}
