//! Helpers shared by integration tests.

/// Forwards the networking methods of `SteamBackend` to `self.fake` (an
/// `Arc<Mutex<FakeBackend>>`), so a wrapper backend can share one fake with a test.
#[allow(unused_macros)]
macro_rules! forward_net {
    () => {
        fn net_init_relay(&mut self) -> SteamResult<()> {
            self.fake.lock().unwrap().net_init_relay()
        }
        fn net_relay_status(&self) -> SteamResult<RelayStatus> {
            self.fake.lock().unwrap().net_relay_status()
        }
        fn net_max_message_size(&self) -> SteamResult<usize> {
            self.fake.lock().unwrap().net_max_message_size()
        }
        fn net_listen_p2p(&mut self, virtual_port: i32) -> SteamResult<NetListenId> {
            self.fake.lock().unwrap().net_listen_p2p(virtual_port)
        }
        fn net_listen_ip(&mut self, addr: std::net::SocketAddr) -> SteamResult<NetListenId> {
            self.fake.lock().unwrap().net_listen_ip(addr)
        }
        fn net_close_listen(&mut self, listen: NetListenId) -> SteamResult<()> {
            self.fake.lock().unwrap().net_close_listen(listen)
        }
        fn net_connect_p2p(&mut self, peer: SteamId, virtual_port: i32) -> SteamResult<NetConnId> {
            self.fake
                .lock()
                .unwrap()
                .net_connect_p2p(peer, virtual_port)
        }
        fn net_connect_ip(&mut self, addr: std::net::SocketAddr) -> SteamResult<NetConnId> {
            self.fake.lock().unwrap().net_connect_ip(addr)
        }
        fn net_accept(&mut self, conn: NetConnId) -> SteamResult<()> {
            self.fake.lock().unwrap().net_accept(conn)
        }
        fn net_close(&mut self, conn: NetConnId, reason: &str, linger: bool) -> SteamResult<()> {
            self.fake.lock().unwrap().net_close(conn, reason, linger)
        }
        fn net_send(
            &mut self,
            conn: NetConnId,
            data: &[u8],
            flags: NetSendFlags,
        ) -> SteamResult<()> {
            self.fake.lock().unwrap().net_send(conn, data, flags)
        }
        fn net_flush(&mut self, conn: NetConnId) -> SteamResult<()> {
            self.fake.lock().unwrap().net_flush(conn)
        }
        fn net_receive(&mut self, conn: NetConnId, max: usize) -> SteamResult<Vec<Vec<u8>>> {
            self.fake.lock().unwrap().net_receive(conn, max)
        }
        fn net_poll_events(&mut self) -> Vec<NetEvent> {
            self.fake.lock().unwrap().net_poll_events()
        }
        fn net_conn_stats(&self, conn: NetConnId) -> Option<NetConnStats> {
            self.fake.lock().unwrap().net_conn_stats(conn)
        }
        fn net_begin_auth(&mut self, user: SteamId, ticket: &[u8]) -> SteamResult<()> {
            self.fake.lock().unwrap().net_begin_auth(user, ticket)
        }
        fn net_end_auth(&mut self, user: SteamId) -> SteamResult<()> {
            self.fake.lock().unwrap().net_end_auth(user)
        }
    };
}
#[allow(unused_imports)]
pub(crate) use forward_net;

/// Implements `LobbyBackend` for a wrapper holding `fake: Arc<Mutex<FakeBackend>>`.
#[allow(unused_macros)]
macro_rules! forward_lobby {
    ($ty:ty) => {
        impl LobbyBackend for $ty {
            fn lobby_create(
                &mut self,
                visibility: LobbyVisibility,
                max_members: u32,
            ) -> SteamResult<()> {
                self.fake
                    .lock()
                    .unwrap()
                    .lobby_create(visibility, max_members)
            }
            fn lobby_join(&mut self, lobby: LobbyId) -> SteamResult<()> {
                self.fake.lock().unwrap().lobby_join(lobby)
            }
            fn lobby_leave(&mut self, lobby: LobbyId) -> SteamResult<()> {
                self.fake.lock().unwrap().lobby_leave(lobby)
            }
            fn lobby_request_list(&mut self, filter: &LobbyFilter) -> SteamResult<()> {
                self.fake.lock().unwrap().lobby_request_list(filter)
            }
            fn lobby_set_data(
                &mut self,
                lobby: LobbyId,
                key: &str,
                value: Option<&str>,
            ) -> SteamResult<()> {
                self.fake.lock().unwrap().lobby_set_data(lobby, key, value)
            }
            fn lobby_info(&self, lobby: LobbyId) -> Option<LobbyInfo> {
                self.fake.lock().unwrap().lobby_info(lobby)
            }
            fn lobby_poll(&mut self) -> Vec<LobbyRaw> {
                self.fake.lock().unwrap().lobby_poll()
            }
        }
    };
}
#[allow(unused_imports)]
pub(crate) use forward_lobby;

/// Forwards the Steam Input methods of `SteamBackend` to `self.fake`.
#[allow(unused_macros)]
macro_rules! forward_input {
    () => {
        fn controllers(&self) -> SteamResult<Vec<ControllerId>> {
            self.fake.lock().unwrap().controllers()
        }
        fn activate_action_set(&mut self, controller: ControllerId, set: &str) -> SteamResult<()> {
            self.fake
                .lock()
                .unwrap()
                .activate_action_set(controller, set)
        }
        fn input_init(&mut self, manifest: Option<&std::path::Path>) -> SteamResult<()> {
            self.fake.lock().unwrap().input_init(manifest)
        }
        fn input_run_frame(&mut self) -> SteamResult<()> {
            self.fake.lock().unwrap().input_run_frame()
        }
        fn input_digital(
            &self,
            controller: ControllerId,
            action: &str,
        ) -> SteamResult<DigitalActionState> {
            self.fake.lock().unwrap().input_digital(controller, action)
        }
        fn input_analog(
            &self,
            controller: ControllerId,
            action: &str,
        ) -> SteamResult<AnalogActionState> {
            self.fake.lock().unwrap().input_analog(controller, action)
        }
        fn input_glyphs(
            &self,
            controller: ControllerId,
            set: &str,
            action: &str,
            kind: InputActionKind,
        ) -> SteamResult<Vec<ActionGlyph>> {
            self.fake
                .lock()
                .unwrap()
                .input_glyphs(controller, set, action, kind)
        }
    };
}
#[allow(unused_imports)]
pub(crate) use forward_input;

/// Forwards the Workshop methods of `SteamBackend` to `self.fake`.
#[allow(unused_macros)]
macro_rules! forward_ugc {
    () => {
        fn subscribed_items(&self) -> SteamResult<Vec<WorkshopItemId>> {
            self.fake.lock().unwrap().subscribed_items()
        }
        fn installed_item(&self, item: WorkshopItemId) -> SteamResult<Option<InstalledItem>> {
            self.fake.lock().unwrap().installed_item(item)
        }
        fn ugc_create_item(&mut self) -> SteamResult<RequestId> {
            self.fake.lock().unwrap().ugc_create_item()
        }
        fn ugc_submit_item(
            &mut self,
            item: WorkshopItemId,
            update: &ItemUpdate,
        ) -> SteamResult<RequestId> {
            self.fake.lock().unwrap().ugc_submit_item(item, update)
        }
        fn ugc_update_progress(&self, request: RequestId) -> Option<UpdateProgress> {
            self.fake.lock().unwrap().ugc_update_progress(request)
        }
        fn ugc_subscribe(
            &mut self,
            item: WorkshopItemId,
            subscribe: bool,
        ) -> SteamResult<RequestId> {
            self.fake.lock().unwrap().ugc_subscribe(item, subscribe)
        }
        fn ugc_query(&mut self, query: &UgcQuery) -> SteamResult<RequestId> {
            self.fake.lock().unwrap().ugc_query(query)
        }
        fn ugc_item_state(&self, item: WorkshopItemId) -> SteamResult<ItemState> {
            self.fake.lock().unwrap().ugc_item_state(item)
        }
        fn ugc_download(&mut self, item: WorkshopItemId, high_priority: bool) -> SteamResult<()> {
            self.fake.lock().unwrap().ugc_download(item, high_priority)
        }
        fn ugc_download_progress(&self, item: WorkshopItemId) -> Option<(u64, u64)> {
            self.fake.lock().unwrap().ugc_download_progress(item)
        }
        fn ugc_poll(&mut self) -> Vec<UgcRaw> {
            self.fake.lock().unwrap().ugc_poll()
        }
    };
}
#[allow(unused_imports)]
pub(crate) use forward_ugc;
