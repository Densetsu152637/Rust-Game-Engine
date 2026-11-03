//! Steam Workshop (user generated content).
//!
//! [`UgcService`] is the game-facing handle. Steam's Workshop calls are asynchronous, so
//! requests return a [`RequestId`] at once and the outcome arrives from
//! [`UgcService::poll`] as [`UgcEvent`]s, in whatever order Steam finishes them. The raw,
//! backend-facing surface is the defaulted `ugc_*` methods on `SteamBackend` plus
//! [`UgcRaw`]; a backend reports finished requests through `ugc_poll`, and the service adds
//! the progress events it derives from `ugc_update_progress` / `ugc_download_progress`.
//!
//! Uploading (`submit_item`) publishes content on the Steam Workshop. Nothing in this
//! crate's automated tests does it; see `docs/steam.md` for the manual procedure.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy_ecs::prelude::Resource;
use engine_project::content_roots::{ContentRoots, RootRevision};

use crate::error::{SteamError, SteamResult};
use crate::session::SteamSession;
use crate::types::{InstalledItem, SteamEvent, SteamId, SteamStatus, WorkshopItemId};

/// Most ids one by-id query may name (Steam's page size).
pub const MAX_QUERY_IDS: usize = 50;
/// Concurrent submissions or downloads the service tracks.
pub const MAX_TRACKED: usize = 64;
// Steam's limits (steamworks-sys `k_cchPublishedDocumentTitleMax` = 129,
// `k_cchPublishedDocumentDescriptionMax` = 8000, `..ChangeDescriptionMax` = 8000 and
// `k_cchTagListMax` = 1025), each counting the terminating NUL.
const MAX_TITLE_BYTES: usize = 128;
const MAX_DESCRIPTION_BYTES: usize = 7999;
const MAX_TAGS: usize = 100;
const MAX_TAG_BYTES: usize = 255;
/// The comma-joined tag list, without the NUL.
const MAX_TAG_LIST_BYTES: usize = 1024;
const MAX_NOTE_BYTES: usize = 7999;
/// How long a submission or download may stay tracked without Steam answering.
const DEFAULT_TRACKING_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// Identifies one asynchronous request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub u64);

/// Who can see a Workshop item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemVisibility {
    Public,
    FriendsOnly,
    Private,
    Unlisted,
}

/// A change to a Workshop item; unset fields are left as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemUpdate {
    pub title: Option<String>,
    pub description: Option<String>,
    pub visibility: Option<ItemVisibility>,
    /// Replaces all tags.
    pub tags: Option<Vec<String>>,
    /// Folder uploaded as the item content.
    pub content_folder: Option<PathBuf>,
    /// Preview image file.
    pub preview: Option<PathBuf>,
    pub change_note: Option<String>,
}

impl ItemUpdate {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn title(mut self, title: &str) -> Self {
        self.title = Some(title.to_owned());
        self
    }
    pub fn description(mut self, description: &str) -> Self {
        self.description = Some(description.to_owned());
        self
    }
    pub fn visibility(mut self, visibility: ItemVisibility) -> Self {
        self.visibility = Some(visibility);
        self
    }
    pub fn tags(mut self, tags: &[&str]) -> Self {
        self.tags = Some(tags.iter().map(|t| (*t).to_owned()).collect());
        self
    }
    pub fn content_folder(mut self, folder: impl Into<PathBuf>) -> Self {
        self.content_folder = Some(folder.into());
        self
    }
    pub fn preview(mut self, file: impl Into<PathBuf>) -> Self {
        self.preview = Some(file.into());
        self
    }
    pub fn change_note(mut self, note: &str) -> Self {
        self.change_note = Some(note.to_owned());
        self
    }

    fn validate(&self) -> SteamResult<()> {
        let bad = |what: &str| Err(SteamError::Rejected(what.to_owned()));
        let text = |s: &str| !s.contains('\0');
        if let Some(title) = &self.title
            && (title.is_empty() || title.len() > MAX_TITLE_BYTES || !text(title))
        {
            return bad("a title needs 1..=128 bytes and no NUL");
        }
        if let Some(d) = &self.description
            && (d.len() > MAX_DESCRIPTION_BYTES || !text(d))
        {
            return bad("a description holds at most 7999 bytes and no NUL");
        }
        if let Some(note) = &self.change_note
            && (note.len() > MAX_NOTE_BYTES || !text(note))
        {
            return bad("a change note holds at most 7999 bytes and no NUL");
        }
        if let Some(tags) = &self.tags {
            if tags.len() > MAX_TAGS {
                return bad("at most 100 tags");
            }
            if tags
                .iter()
                .any(|t| t.is_empty() || t.len() > MAX_TAG_BYTES || t.contains([',', '\0']))
            {
                return bad("a tag needs 1..=255 bytes without commas or NUL");
            }
            let joined = tags.iter().map(String::len).sum::<usize>() + tags.len().saturating_sub(1);
            if joined > MAX_TAG_LIST_BYTES {
                return bad("the tags together (with separators) hold at most 1024 bytes");
            }
        }
        if let Some(folder) = &self.content_folder
            && !folder.is_dir()
        {
            return bad("the content folder does not exist");
        }
        if let Some(file) = &self.preview
            && !file.is_file()
        {
            return bad("the preview file does not exist");
        }
        for path in [&self.content_folder, &self.preview].into_iter().flatten() {
            if path.to_str().is_none_or(|p| p.contains('\0')) {
                return bad("paths must be UTF-8 without NUL");
            }
        }
        Ok(())
    }
}

/// Which phase of a submission Steam is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateStage {
    Invalid,
    PreparingConfig,
    PreparingContent,
    UploadingContent,
    UploadingPreview,
    CommittingChanges,
}

/// Progress of a submission: `done` of `total` bytes in the current stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateProgress {
    pub stage: UpdateStage,
    pub done: u64,
    pub total: u64,
}

/// A Workshop item as reported by a query.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkshopItem {
    pub id: WorkshopItemId,
    pub title: String,
    pub description: String,
    pub owner: SteamId,
    pub visibility: ItemVisibility,
    pub tags: Vec<String>,
    pub time_created: u32,
    pub time_updated: u32,
    pub file_size: u32,
    pub votes_up: u32,
    pub votes_down: u32,
    pub subscriptions: u64,
    pub banned: bool,
}

/// Ordering of a public query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicSort {
    MostVoted,
    Newest,
    MostSubscribed,
    Trending,
    /// Needs [`PublicQuery::search`].
    TextSearch,
}

/// A basic query of the app's public Workshop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicQuery {
    pub sort: PublicSort,
    pub search: Option<String>,
    /// Items must carry every one of these tags.
    pub required_tags: Vec<String>,
    /// 1-based page; Steam returns up to 50 items per page.
    pub page: u32,
}

impl Default for PublicQuery {
    fn default() -> Self {
        Self {
            sort: PublicSort::MostVoted,
            search: None,
            required_tags: Vec::new(),
            page: 1,
        }
    }
}

/// What a query asks the backend for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UgcQuery {
    ById(Vec<WorkshopItemId>),
    Public(PublicQuery),
}

/// One page of query results.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QueryPage {
    pub items: Vec<WorkshopItem>,
    /// Matches across all pages.
    pub total: u32,
}

/// Local state of an item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ItemState {
    pub subscribed: bool,
    pub installed: bool,
    pub needs_update: bool,
    pub downloading: bool,
    pub download_pending: bool,
}

/// A finished request, as a backend reports it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum UgcRaw {
    /// `(item, user must accept the Workshop legal agreement)`.
    Created {
        request: RequestId,
        result: Result<(WorkshopItemId, bool), String>,
    },
    Submitted {
        request: RequestId,
        result: Result<(WorkshopItemId, bool), String>,
    },
    Subscription {
        request: RequestId,
        item: WorkshopItemId,
        subscribed: bool,
        result: Result<(), String>,
    },
    Queried {
        request: RequestId,
        result: Result<QueryPage, String>,
    },
    /// Steam finished (or failed) downloading an item.
    Downloaded {
        item: WorkshopItemId,
        result: Result<(), String>,
    },
}

/// Something the Workshop service reports from [`UgcService::poll`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum UgcEvent {
    /// A request finished.
    Reply(UgcRaw),
    /// A submission moved to a new stage or byte count.
    UpdateProgress {
        request: RequestId,
        item: WorkshopItemId,
        progress: UpdateProgress,
    },
    /// A download moved forward (bytes).
    DownloadProgress {
        item: WorkshopItemId,
        done: u64,
        total: u64,
    },
}

/// Game-facing Workshop handle.
#[derive(Resource)]
pub struct UgcService {
    session: SteamSession,
    submits: BTreeMap<RequestId, (WorkshopItemId, Option<UpdateProgress>, Instant)>,
    downloads: BTreeMap<WorkshopItemId, (Option<(u64, u64)>, Instant)>,
    /// Items whose install was announced from their state; swallows the late callback.
    retired: BTreeSet<WorkshopItemId>,
    timeout: Duration,
}

impl UgcService {
    /// A service over a ready session.
    pub fn new(session: SteamSession) -> SteamResult<Self> {
        match session.status() {
            SteamStatus::Ready { .. } => Ok(Self {
                session,
                submits: BTreeMap::new(),
                downloads: BTreeMap::new(),
                retired: BTreeSet::new(),
                timeout: DEFAULT_TRACKING_TIMEOUT,
            }),
            other => Err(SteamError::Unavailable(
                other
                    .unavailable_reason()
                    .unwrap_or(crate::UnavailableReason::NotRunning),
            )),
        }
    }

    /// How long a submission or download may stay unanswered before it ends with an error.
    pub fn set_tracking_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Creates an empty (private) Workshop item for this app. Reply: [`UgcRaw::Created`].
    /// The item is invisible to others until a submission makes it public.
    pub fn create_item(&mut self) -> SteamResult<RequestId> {
        self.session.with_backend(|b| b.ugc_create_item())
    }

    /// Updates an existing item and uploads it. **This publishes to the Steam Workshop**
    /// (according to the item's visibility); only call it from a deliberate, manual flow.
    /// Progress arrives as [`UgcEvent::UpdateProgress`], the result as [`UgcRaw::Submitted`].
    pub fn submit_item(
        &mut self,
        item: WorkshopItemId,
        update: &ItemUpdate,
    ) -> SteamResult<RequestId> {
        update.validate()?;
        if self.submits.len() >= MAX_TRACKED {
            return Err(SteamError::Rejected(
                "too many submissions in flight".into(),
            ));
        }
        let request = self
            .session
            .with_backend(|b| b.ugc_submit_item(item, update))?;
        self.submits.insert(request, (item, None, Instant::now()));
        Ok(request)
    }

    pub fn subscribe(&mut self, item: WorkshopItemId) -> SteamResult<RequestId> {
        self.session.with_backend(|b| b.ugc_subscribe(item, true))
    }

    pub fn unsubscribe(&mut self, item: WorkshopItemId) -> SteamResult<RequestId> {
        self.session.with_backend(|b| b.ugc_subscribe(item, false))
    }

    /// Queries the given items (at most [`MAX_QUERY_IDS`]). Reply: [`UgcRaw::Queried`].
    pub fn query_items(&mut self, ids: &[WorkshopItemId]) -> SteamResult<RequestId> {
        if ids.is_empty() || ids.len() > MAX_QUERY_IDS {
            return Err(SteamError::Rejected(format!(
                "a by-id query needs 1..={MAX_QUERY_IDS} ids"
            )));
        }
        self.session
            .with_backend(|b| b.ugc_query(&UgcQuery::ById(ids.to_vec())))
    }

    /// Queries the player's subscribed items (the first [`MAX_QUERY_IDS`]). When there are
    /// none, the reply is an empty page delivered by the next `poll`.
    pub fn query_subscribed(&mut self) -> SteamResult<RequestId> {
        let mut ids = self.subscribed()?;
        ids.truncate(MAX_QUERY_IDS);
        // An empty list is answered with an empty page, without asking Steam.
        self.session
            .with_backend(|b| b.ugc_query(&UgcQuery::ById(ids)))
    }

    /// Queries the app's public Workshop.
    pub fn query_public(&mut self, query: &PublicQuery) -> SteamResult<RequestId> {
        let search_ok = query
            .search
            .as_ref()
            .is_none_or(|s| s.len() <= 255 && !s.contains('\0'));
        if query.page == 0
            || !search_ok
            || query.required_tags.len() > MAX_TAGS
            || query
                .required_tags
                .iter()
                .any(|t| t.is_empty() || t.len() > MAX_TAG_BYTES || t.contains('\0'))
            || (query.sort == PublicSort::TextSearch && query.search.is_none())
        {
            return Err(SteamError::Rejected("invalid public query".into()));
        }
        self.session
            .with_backend(|b| b.ugc_query(&UgcQuery::Public(query.clone())))
    }

    /// Ids of the subscribed items.
    pub fn subscribed(&self) -> SteamResult<Vec<WorkshopItemId>> {
        self.session.with_backend(|b| b.subscribed_items())
    }

    /// Install details of an item, or `None` when it is not subscribed or not installed.
    pub fn installed(&self, item: WorkshopItemId) -> SteamResult<Option<InstalledItem>> {
        self.session.with_backend(|b| b.installed_item(item))
    }

    pub fn item_state(&self, item: WorkshopItemId) -> SteamResult<ItemState> {
        self.session.with_backend(|b| b.ugc_item_state(item))
    }

    /// Asks Steam to download (or update) a subscribed item. Progress arrives as
    /// [`UgcEvent::DownloadProgress`], completion as [`UgcRaw::Downloaded`] (and a
    /// `SteamEvent::WorkshopItemInstalled` from the session on success).
    pub fn download_item(&mut self, item: WorkshopItemId, high_priority: bool) -> SteamResult<()> {
        if self.downloads.len() >= MAX_TRACKED && !self.downloads.contains_key(&item) {
            return Err(SteamError::Rejected("too many downloads in flight".into()));
        }
        self.session
            .with_backend(|b| b.ugc_download(item, high_priority))?;
        // A new download supersedes an earlier state-based retirement: the callback that
        // may follow belongs to this one and must announce the install.
        self.retired.remove(&item);
        self.downloads.entry(item).or_insert((None, Instant::now()));
        Ok(())
    }

    /// Collects finished requests and progress changes. Call once per tick, after the
    /// session pumped Steam callbacks.
    ///
    /// A tracked download also ends when Steam reports the item installed, up to date and
    /// no longer downloading (Steam sends no callback for an already current item), and a
    /// submission or download that Steam has not answered within the tracking timeout
    /// (default two hours) ends with an error reply, so nothing is tracked forever.
    pub fn poll(&mut self) -> Vec<UgcEvent> {
        let mut events = Vec::new();
        let mut installed = Vec::new();
        {
            let mut state = self.session.lock();
            let backend = state.backend.as_mut();
            let raws = backend.ugc_poll();
            let mut timed_out = Vec::new();
            for (request, (item, last, started)) in &mut self.submits {
                if raws.iter().any(
                    |r| matches!(r, UgcRaw::Submitted { request: done, .. } if done == request),
                ) {
                    continue;
                }
                if started.elapsed() >= self.timeout {
                    timed_out.push(*request);
                    continue;
                }
                if let Some(progress) = backend.ugc_update_progress(*request)
                    && *last != Some(progress)
                {
                    *last = Some(progress);
                    events.push(UgcEvent::UpdateProgress {
                        request: *request,
                        item: *item,
                        progress,
                    });
                }
            }
            for request in timed_out {
                self.submits.remove(&request);
                backend.ugc_forget_update(request);
                events.push(UgcEvent::Reply(UgcRaw::Submitted {
                    request,
                    result: Err("Steam did not answer the submission in time".into()),
                }));
            }
            let mut finished = Vec::new();
            for (item, (last, started)) in &mut self.downloads {
                if raws
                    .iter()
                    .any(|r| matches!(r, UgcRaw::Downloaded { item: done, .. } if done == item))
                {
                    continue;
                }
                let state_now = backend.ugc_item_state(*item).ok();
                if let Some(s) = state_now
                    && s.installed
                    && !s.needs_update
                    && !s.downloading
                    && !s.download_pending
                {
                    finished.push((*item, Ok(())));
                } else if started.elapsed() >= self.timeout {
                    finished.push((
                        *item,
                        Err("Steam did not finish the download in time".into()),
                    ));
                } else if let Some(now) = backend.ugc_download_progress(*item)
                    && *last != Some(now)
                {
                    *last = Some(now);
                    events.push(UgcEvent::DownloadProgress {
                        item: *item,
                        done: now.0,
                        total: now.1,
                    });
                }
            }
            for (item, result) in finished {
                self.downloads.remove(&item);
                if result.is_ok() {
                    installed.push(item);
                    if self.retired.len() >= MAX_TRACKED {
                        self.retired.clear();
                    }
                    // A callback for the same download may still arrive; do not announce
                    // the install twice.
                    self.retired.insert(item);
                }
                events.push(UgcEvent::Reply(UgcRaw::Downloaded { item, result }));
            }
            for raw in raws {
                match &raw {
                    UgcRaw::Submitted { request, .. } => {
                        self.submits.remove(request);
                    }
                    UgcRaw::Downloaded { item, result } => {
                        self.downloads.remove(item);
                        if result.is_ok() && !self.retired.remove(item) {
                            installed.push(*item);
                        }
                    }
                    _ => {}
                }
                events.push(UgcEvent::Reply(raw));
            }
            state
                .pending
                .extend(installed.into_iter().map(SteamEvent::WorkshopItemInstalled));
        }
        events
    }
}

/// What [`WorkshopContent`] did with a Workshop item.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkshopContentEvent {
    /// The item folder was mounted as `@workshop-<id>/...`.
    Mounted {
        item: WorkshopItemId,
        revision: RootRevision,
    },
    /// An updated item folder replaced the previous mount.
    Replaced {
        item: WorkshopItemId,
        revision: RootRevision,
    },
    /// The mount was removed. `reason` is `None` for a completed unsubscribe; otherwise the
    /// item is no longer subscribed, or an update failed validation and the stale mount was
    /// dropped instead of serving the old version against the new files.
    Unmounted {
        item: WorkshopItemId,
        reason: Option<String>,
    },
    /// The item folder was not mounted (it failed validation); the game carries on.
    Skipped {
        item: WorkshopItemId,
        reason: String,
    },
}

/// Mounts installed Workshop items as read-only content roots.
///
/// Feed it the events of [`UgcService::poll`]: a finished download mounts (or replaces) the
/// item as mount `workshop-<id>`, an unsubscribe removes it, and an item whose folder fails
/// validation is skipped and reported. At startup call [`WorkshopContent::mount_subscribed`].
/// Item content is other players' data: it is mounted under the content-root rules (leaf
/// kinds only, no links, confined below the folder Steam installs items into).
pub struct WorkshopContent {
    roots: ContentRoots,
    mounted: BTreeSet<WorkshopItemId>,
}

impl WorkshopContent {
    /// Mounts into `roots` (usually `project.content_roots`).
    pub fn new(roots: ContentRoots) -> Self {
        Self {
            roots,
            mounted: BTreeSet::new(),
        }
    }

    /// The mount name of an item.
    pub fn mount_name(item: WorkshopItemId) -> String {
        format!("workshop-{}", item.0)
    }

    /// The full asset id of `id` inside an item's content root.
    pub fn asset_id(item: WorkshopItemId, id: &str) -> String {
        format!("@{}/{id}", Self::mount_name(item))
    }

    /// Mounts every subscribed item that Steam reports installed, after dropping mounts of
    /// items that are no longer subscribed (see [`WorkshopContent::reconcile`]).
    pub fn mount_subscribed(&mut self, ugc: &UgcService) -> Vec<WorkshopContentEvent> {
        let mut out = self.reconcile(ugc);
        let items = ugc.subscribed().unwrap_or_default();
        out.extend(items.into_iter().filter_map(|item| self.mount(ugc, item)));
        out
    }

    /// Unmounts items that are mounted but no longer subscribed, so an unsubscribe made
    /// outside the game (the Steam client, another machine) is picked up. Call it
    /// occasionally, for instance at startup or when the game regains focus. It does
    /// nothing when Steam cannot list the subscriptions. A successful but transiently
    /// empty list unmounts every item; the next [`mount_subscribed`](Self::mount_subscribed)
    /// remounts the installed ones.
    pub fn reconcile(&mut self, ugc: &UgcService) -> Vec<WorkshopContentEvent> {
        let Ok(subscribed) = ugc.subscribed() else {
            return Vec::new();
        };
        let gone: Vec<_> = self
            .mounted
            .iter()
            .copied()
            .filter(|item| !subscribed.contains(item))
            .collect();
        gone.into_iter()
            .map(|item| {
                self.mounted.remove(&item);
                self.roots.unregister(&Self::mount_name(item));
                WorkshopContentEvent::Unmounted {
                    item,
                    reason: Some("the item is no longer subscribed".into()),
                }
            })
            .collect()
    }

    /// Reacts to the events of one [`UgcService::poll`].
    pub fn process(&mut self, ugc: &UgcService, events: &[UgcEvent]) -> Vec<WorkshopContentEvent> {
        let mut out = Vec::new();
        for event in events {
            match event {
                UgcEvent::Reply(UgcRaw::Downloaded {
                    item,
                    result: Ok(()),
                }) => out.extend(self.mount(ugc, *item)),
                UgcEvent::Reply(UgcRaw::Subscription {
                    item,
                    subscribed: false,
                    result: Ok(()),
                    ..
                }) => {
                    self.mounted.remove(item);
                    if self.roots.unregister(&Self::mount_name(*item)) {
                        out.push(WorkshopContentEvent::Unmounted {
                            item: *item,
                            reason: None,
                        });
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn mount(&mut self, ugc: &UgcService, item: WorkshopItemId) -> Option<WorkshopContentEvent> {
        let info = ugc.installed(item).ok()??;
        // An update is still downloading: keep the mounted version until it finishes.
        if info.needs_update && self.mounted.contains(&item) {
            return None;
        }
        // A folder that fails validation is skipped; if the item was mounted, its stale mount
        // is dropped too so the old manifest is never served against the new files.
        let skipped = |this: &mut Self, reason: String| {
            if this.mounted.remove(&item) {
                this.roots.unregister(&Self::mount_name(item));
                return Some(WorkshopContentEvent::Unmounted {
                    item,
                    reason: Some(format!("the updated item failed validation: {reason}")),
                });
            }
            Some(WorkshopContentEvent::Skipped { item, reason })
        };
        let Some(parent) = info.path.parent() else {
            return skipped(self, "the install folder has no parent".into());
        };
        match self
            .roots
            .register(&Self::mount_name(item), &info.path, parent)
        {
            Ok(revision) => {
                let replaced = !self.mounted.insert(item);
                Some(if replaced {
                    WorkshopContentEvent::Replaced { item, revision }
                } else {
                    WorkshopContentEvent::Mounted { item, revision }
                })
            }
            Err(error) => {
                let reason = error
                    .diagnostics
                    .first()
                    .map(|d| format!("{}: {}", d.code, d.message))
                    .unwrap_or_else(|| "registration failed".into());
                skipped(self, reason)
            }
        }
    }
}
