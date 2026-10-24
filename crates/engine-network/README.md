# engine-network

Opt-in authoritative TCP replication with real nonblocking sockets, bounded JSON
frames and queues, input ownership checks, and TRS snapshot interpolation.

See [networking](../../docs/networking.md) for usage, limits, protocol, and trust boundaries.

```powershell
cargo test -p engine-network
cargo clippy -p engine-network --all-targets -- -D warnings
```
