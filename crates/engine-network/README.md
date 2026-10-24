# engine-network

Opt-in authoritative replication with mutual TLS 1.3 and native certificate
authorization, bounded nonblocking sockets/queues, input ownership checks, and
TRS snapshot interpolation. Explicit plaintext APIs remain for trusted LANs.

See [networking](../../docs/networking.md) for usage, limits, protocol, and trust boundaries.

```powershell
cargo test -p engine-network
cargo clippy -p engine-network --all-targets -- -D warnings
cargo run -p engine-network --example secure_loopback
```
