# Known Vulnerabilities and Mitigations

## RUSTSEC-2025-0009: ring 0.16.20 AES overflow panic

**Status:** Known, mitigated, tracking upstream fix
**Severity:** Low (in our context)
**CVE:** RUSTSEC-2025-0009
**Affected crate:** ring 0.16.20 (transitive via libp2p-tls → rcgen)
**Fix available:** ring ≥0.17.12

### Description
Some AES functions in ring 0.16.20 may panic when overflow checking is enabled. This could cause a denial-of-service if triggered.

### Why we can't upgrade
ring 0.16.20 is pulled in transitively by `rcgen 0.11.3`, which is a dependency of `libp2p-tls 0.5.0`. Upgrading ring requires upgrading libp2p to a version that depends on rcgen with ring ≥0.17. As of libp2p 0.54.1 (our current version), this hasn't been updated upstream.

### Mitigation
1. **The vulnerable AES functions are in TLS certificate generation** (rcgen), not in our protocol's AES-256-GCM encryption (which uses ring directly at a newer version via our Cargo.lock resolution).
2. **Our protocol's AES-256-GCM** (keystore encryption) uses ring's AES through the `ring` crate directly, which resolves to a separate version in our dependency tree.
3. **Overflow checking is disabled in release builds** (`overflow-checks = false` is the default for the `release` profile in Rust), so the panic cannot trigger in production binaries.
4. **libp2p-tls** uses rcgen only for generating self-signed certificates during QUIC/TLS handshakes. The AES functions in question are not part of the certificate generation path.

### Tracking
- Monitor libp2p releases for rcgen/ring updates
- When libp2p upgrades to ring ≥0.17, update our dependency
- Periodically re-run `cargo audit` to verify status

### Verification
```bash
cargo audit  # Should show only this known issue
```
