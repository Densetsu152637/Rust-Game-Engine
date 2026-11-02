//! Workshop half of the real backend over `steamworks::UGC` (steamworks 0.13.1).
//!
//! Requests are asynchronous: their replies are pushed into a shared queue by call-result
//! closures (run inside `Client::run_callbacks`) and drained by `ugc_poll`. Submissions keep
//! their `UpdateWatchHandle` so progress can be read until the reply arrives.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};

use steamworks::{
    AppIDs, Client, FileType, ItemState as RawState, PublishedFileId, PublishedFileVisibility,
    QueryResults, UGCQueryType, UGCStatisticType, UGCType, UpdateStatus, UpdateWatchHandle,
};

use crate::backend::SteamBackend;
use crate::error::{SteamError, SteamResult};
use crate::real::RealBackend;
use crate::types::{AppId, InstalledItem, SteamId, SteamStatus, WorkshopItemId};
use crate::workshop::{
    ItemState, ItemUpdate, ItemVisibility, PublicQuery, PublicSort, QueryPage, RequestId, UgcQuery,
    UgcRaw, UpdateProgress, UpdateStage, WorkshopItem,
};

/// Most replies held between polls. Replies to requests are never evicted: new requests
/// are refused once `REQUEST_ROOM` replies wait, and only unsolicited download
/// notifications (Steam downloads subscribed items on its own) are dropped when full.
const MAX_REPLIES: usize = 256;
const REQUEST_ROOM: usize = 128;

type Replies = Arc<Mutex<Vec<UgcRaw>>>;

#[derive(Default)]
pub(crate) struct RealUgc {
    replies: Replies,
    updates: BTreeMap<RequestId, UpdateWatchHandle>,
    next: u64,
}

fn push(replies: &Replies, raw: UgcRaw) {
    let mut queue = replies
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if queue.len() >= MAX_REPLIES {
        if matches!(raw, UgcRaw::Downloaded { .. }) {
            return;
        }
        if let Some(at) = queue
            .iter()
            .position(|r| matches!(r, UgcRaw::Downloaded { .. }))
        {
            queue.remove(at);
        }
    }
    queue.push(raw);
}

fn check_room(ugc: &RealUgc) -> SteamResult<()> {
    let waiting = ugc
        .replies
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len();
    if waiting >= REQUEST_ROOM {
        Err(SteamError::Rejected(
            "too many Workshop replies are waiting to be polled".into(),
        ))
    } else {
        Ok(())
    }
}

impl RealUgc {
    /// Drops the update handles, which hold the Steam client alive.
    pub(crate) fn release_updates(&mut self) {
        self.updates.clear();
    }
}

/// Registers the download-finished notification.
pub(crate) fn register(client: &Client, ugc: &RealUgc) -> Vec<steamworks::CallbackHandle> {
    let replies = Arc::clone(&ugc.replies);
    let handle = client.register_callback(move |cb: steamworks::DownloadItemResult| {
        let result = match cb.error {
            None => Ok(()),
            Some(error) => Err(error.to_string()),
        };
        push(
            &replies,
            UgcRaw::Downloaded {
                item: WorkshopItemId(cb.published_file_id.0),
                result,
            },
        );
    });
    vec![handle]
}

fn app_id(backend: &RealBackend) -> SteamResult<AppId> {
    match backend.status() {
        SteamStatus::Ready { app_id, .. } => Ok(app_id),
        _ => Err(backend.unavailable()),
    }
}

fn reject_nul(text: &str) -> SteamResult<()> {
    if text.contains('\0') {
        Err(SteamError::InvalidName(text.replace('\0', "\\0")))
    } else {
        Ok(())
    }
}

fn next_request(ugc: &mut RealUgc) -> SteamResult<RequestId> {
    check_room(ugc)?;
    ugc.next += 1;
    Ok(RequestId(ugc.next))
}

fn visibility_in(v: ItemVisibility) -> PublishedFileVisibility {
    match v {
        ItemVisibility::Public => PublishedFileVisibility::Public,
        ItemVisibility::FriendsOnly => PublishedFileVisibility::FriendsOnly,
        ItemVisibility::Private => PublishedFileVisibility::Private,
        ItemVisibility::Unlisted => PublishedFileVisibility::Unlisted,
    }
}

fn visibility_out(v: PublishedFileVisibility) -> ItemVisibility {
    match v {
        PublishedFileVisibility::Public => ItemVisibility::Public,
        PublishedFileVisibility::FriendsOnly => ItemVisibility::FriendsOnly,
        PublishedFileVisibility::Private => ItemVisibility::Private,
        PublishedFileVisibility::Unlisted => ItemVisibility::Unlisted,
    }
}

pub(crate) fn create_item(backend: &mut RealBackend) -> SteamResult<RequestId> {
    let app = app_id(backend)?;
    let request = next_request(&mut backend.ugc)?;
    let replies = Arc::clone(&backend.ugc.replies);
    backend.client()?.ugc().create_item(
        steamworks::AppId(app.0),
        FileType::Community,
        move |result| {
            let result = result
                .map(|(id, legal)| (WorkshopItemId(id.0), legal))
                .map_err(|e| e.to_string());
            push(&replies, UgcRaw::Created { request, result });
        },
    );
    Ok(request)
}

pub(crate) fn submit_item(
    backend: &mut RealBackend,
    item: WorkshopItemId,
    update: &ItemUpdate,
) -> SteamResult<RequestId> {
    let app = app_id(backend)?;
    for text in [&update.title, &update.description, &update.change_note]
        .into_iter()
        .flatten()
    {
        reject_nul(text)?;
    }
    for tag in update.tags.iter().flatten() {
        reject_nul(tag)?;
    }
    let request = next_request(&mut backend.ugc)?;
    let replies = Arc::clone(&backend.ugc.replies);
    let ugc = backend.client()?.ugc();
    // The binding's setters `assert!` on Steam's answer and `unwrap` path
    // canonicalisation, so a value Steam dislikes (or a folder that vanished) would panic.
    // Building happens on this thread, outside any Steam callback; contain it and refuse.
    // On Windows the binding passes Steam the canonical path with a `\\?\` prefix; the
    // binding offers no way to change that.
    let built = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut handle = ugc.start_item_update(steamworks::AppId(app.0), PublishedFileId(item.0));
        if let Some(title) = &update.title {
            handle = handle.title(title);
        }
        if let Some(description) = &update.description {
            handle = handle.description(description);
        }
        if let Some(visibility) = update.visibility {
            handle = handle.visibility(visibility_in(visibility));
        }
        if let Some(tags) = &update.tags {
            handle = handle.tags(tags.clone(), false);
        }
        if let Some(folder) = &update.content_folder {
            handle = handle.content_path(folder);
        }
        if let Some(preview) = &update.preview {
            handle = handle.preview_path(preview);
        }
        handle.submit(update.change_note.as_deref(), move |result| {
            let result = result
                .map(|(id, legal)| (WorkshopItemId(id.0), legal))
                .map_err(|e| e.to_string());
            push(&replies, UgcRaw::Submitted { request, result });
        })
    }));
    let Ok(watch) = built else {
        return Err(SteamError::Rejected(
            "the Steam binding refused part of the update (a value or path was rejected)".into(),
        ));
    };
    backend.ugc.updates.insert(request, watch);
    Ok(request)
}

pub(crate) fn update_progress(backend: &RealBackend, request: RequestId) -> Option<UpdateProgress> {
    let (status, done, total) = backend.ugc.updates.get(&request)?.progress();
    let stage = match status {
        UpdateStatus::Invalid => UpdateStage::Invalid,
        UpdateStatus::PreparingConfig => UpdateStage::PreparingConfig,
        UpdateStatus::PreparingContent => UpdateStage::PreparingContent,
        UpdateStatus::UploadingContent => UpdateStage::UploadingContent,
        UpdateStatus::UploadingPreviewFile => UpdateStage::UploadingPreview,
        UpdateStatus::CommittingChanges => UpdateStage::CommittingChanges,
    };
    Some(UpdateProgress { stage, done, total })
}

pub(crate) fn subscribe(
    backend: &mut RealBackend,
    item: WorkshopItemId,
    subscribe: bool,
) -> SteamResult<RequestId> {
    let request = next_request(&mut backend.ugc)?;
    let replies = Arc::clone(&backend.ugc.replies);
    let ugc = backend.client()?.ugc();
    let done = move |result: Result<(), steamworks::SteamError>| {
        push(
            &replies,
            UgcRaw::Subscription {
                request,
                item,
                subscribed: subscribe,
                result: result.map_err(|e| e.to_string()),
            },
        );
    };
    if subscribe {
        ugc.subscribe_item(PublishedFileId(item.0), done);
    } else {
        ugc.unsubscribe_item(PublishedFileId(item.0), done);
    }
    Ok(request)
}

pub(crate) fn query(backend: &mut RealBackend, query: &UgcQuery) -> SteamResult<RequestId> {
    let app = app_id(backend)?;
    let request = next_request(&mut backend.ugc)?;
    let replies = Arc::clone(&backend.ugc.replies);
    let ugc = backend.client()?.ugc();
    let handle = match query {
        UgcQuery::ById(ids) if ids.is_empty() => {
            push(
                &replies,
                UgcRaw::Queried {
                    request,
                    result: Ok(QueryPage::default()),
                },
            );
            return Ok(request);
        }
        UgcQuery::ById(ids) => {
            ugc.query_items(ids.iter().map(|id| PublishedFileId(id.0)).collect())
        }
        UgcQuery::Public(q) => public_query(&ugc, steamworks::AppId(app.0), q)?,
    }
    .map_err(|_| SteamError::Rejected("Steam refused to create the query".into()))?;
    let done = replies;
    handle.fetch(move |result| {
        // This closure runs inside Steam's callback dispatch: a panic must never unwind
        // out of it. The binding's `QueryResults::get` panics on a visibility or file type
        // it does not know, so convert under `catch_unwind` and answer with an error.
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| convert_page(result)))
            .unwrap_or_else(|_| {
                Err(
                    "the Steam binding could not read an item (unknown visibility or file type)"
                        .to_owned(),
                )
            });
        push(&done, UgcRaw::Queried { request, result });
    });
    Ok(request)
}

fn convert_page(
    result: Result<QueryResults<'_>, steamworks::SteamError>,
) -> Result<QueryPage, String> {
    let results = result.map_err(|e| e.to_string())?;
    let items = (0..results.returned_results())
        .filter_map(|at| results.get(at).map(|r| (at, r)))
        .map(|(at, r)| WorkshopItem {
            id: WorkshopItemId(r.published_file_id.0),
            title: r.title,
            description: r.description,
            owner: SteamId(r.owner.raw()),
            visibility: visibility_out(r.visibility),
            tags: r.tags.into_iter().filter(|t| !t.is_empty()).collect(),
            time_created: r.time_created,
            time_updated: r.time_updated,
            file_size: r.file_size,
            votes_up: r.num_upvotes,
            votes_down: r.num_downvotes,
            subscriptions: results
                .statistic(at, UGCStatisticType::Subscriptions)
                .unwrap_or(0),
            banned: r.banned,
        })
        .collect();
    Ok(QueryPage {
        items,
        total: results.total_results(),
    })
}

fn public_query(
    ugc: &steamworks::UGC,
    app: steamworks::AppId,
    q: &PublicQuery,
) -> SteamResult<Result<steamworks::QueryHandle, steamworks::CreateQueryError>> {
    if let Some(search) = &q.search {
        reject_nul(search)?;
    }
    for tag in &q.required_tags {
        reject_nul(tag)?;
    }
    let kind = match q.sort {
        PublicSort::MostVoted => UGCQueryType::RankedByVote,
        PublicSort::Newest => UGCQueryType::RankedByPublicationDate,
        PublicSort::MostSubscribed => UGCQueryType::RankedByTotalUniqueSubscriptions,
        PublicSort::Trending => UGCQueryType::RankedByTrend,
        PublicSort::TextSearch => UGCQueryType::RankedByTextSearch,
    };
    Ok(ugc
        .query_all(kind, UGCType::Items, AppIDs::ConsumerAppId(app), q.page)
        .map(|mut handle| {
            for tag in &q.required_tags {
                handle = handle.add_required_tag(tag);
            }
            if let Some(search) = &q.search {
                handle = handle.set_search_text(search);
            }
            handle
        }))
}

pub(crate) fn item_state(backend: &RealBackend, item: WorkshopItemId) -> SteamResult<ItemState> {
    let state = backend.client()?.ugc().item_state(PublishedFileId(item.0));
    Ok(ItemState {
        subscribed: state.contains(RawState::SUBSCRIBED),
        installed: state.contains(RawState::INSTALLED),
        needs_update: state.contains(RawState::NEEDS_UPDATE),
        downloading: state.contains(RawState::DOWNLOADING),
        download_pending: state.contains(RawState::DOWNLOAD_PENDING),
    })
}

pub(crate) fn download(backend: &RealBackend, item: WorkshopItemId, high: bool) -> SteamResult<()> {
    if backend
        .client()?
        .ugc()
        .download_item(PublishedFileId(item.0), high)
    {
        Ok(())
    } else {
        Err(SteamError::Rejected(
            "Steam refused the download (not subscribed or invalid item)".into(),
        ))
    }
}

pub(crate) fn download_progress(backend: &RealBackend, item: WorkshopItemId) -> Option<(u64, u64)> {
    backend
        .client()
        .ok()?
        .ugc()
        .item_download_info(PublishedFileId(item.0))
}

pub(crate) fn subscribed(backend: &RealBackend) -> SteamResult<Vec<WorkshopItemId>> {
    Ok(backend
        .client()?
        .ugc()
        .subscribed_items(false)
        .into_iter()
        .map(|id| WorkshopItemId(id.0))
        .collect())
}

pub(crate) fn installed(
    backend: &RealBackend,
    item: WorkshopItemId,
) -> SteamResult<Option<InstalledItem>> {
    let ugc = backend.client()?.ugc();
    let id = PublishedFileId(item.0);
    let needs_update = ugc.item_state(id).contains(RawState::NEEDS_UPDATE);
    Ok(ugc.item_install_info(id).map(|info| InstalledItem {
        id: item,
        path: info.folder.into(),
        size_bytes: info.size_on_disk,
        timestamp: info.timestamp,
        needs_update,
    }))
}

pub(crate) fn poll(backend: &mut RealBackend) -> Vec<UgcRaw> {
    let raws = std::mem::take(
        &mut *backend
            .ugc
            .replies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    );
    for raw in &raws {
        if let UgcRaw::Submitted { request, .. } = raw {
            backend.ugc.updates.remove(request);
        }
    }
    raws
}

pub(crate) fn forget_update(backend: &mut RealBackend, request: RequestId) {
    backend.ugc.updates.remove(&request);
}
