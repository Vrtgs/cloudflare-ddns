# Changelog

All notable changes to this project will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org).

## [0.3.7] - 2026-7-10
- Resolve IPv4 and IPv6 independently, each from its own set of sources
- Detect the local IPv6 address natively (via OS APIs) before falling back
  to external IP-lookup services, preferring stable addresses over
  temporary/privacy ones
- Add per-source IP type configuration (`type = "v4" | "v6" | "any"`) for
  `sources.toml`
- Update default IP sources: drop `api64.ipify.org`; add explicit IPv4/IPv6
  pairs for ipify, ident.me, tnedi.me, icanhazip, nsupdate.info, and ipinfo.io
- Watch `http.toml` and `misc.toml` for live changes; `api.toml` no longer
  requires a restart to apply
- Avoid infinite restart loop on repeated panics

## [0.3.6] - 2026-7-1
- Fix linux dispatcher saving / placing logic

## [0.3.5] - 2026-7-1
- Fix linux native NetworkManager OS notifications and network status check
- Update version number in CLI
- Update dependencies

## [0.3.4] - 2026-6-30
- Add better diagnostic messages and update reporting
- update dependencies

## [0.3.3] - 2025-10-24
- Fix bug where the program would think its all out of IP's even when it wasn't.

## [0.3.2] - 2025-10-24
- Fix issue where record one type of ip logs is missing, and that keeps causing failure for BOTH ip record types.

## [0.3.1] - 2025-04-30
- simultaneous Ipv6 and Ipv4 support.

## [0.3.0] - 2024-08-16
- Ipv6 support.

## [0.2.0] - 2024-08-16
- Remove experimental wasm runtime.
- improve executable size.
- Finally, add a README.md.

## [0.1.0] - 2024-06-18
- Initial release.
