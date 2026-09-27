# Third-party runtime notices

The easymusic release archives redistribute these unmodified upstream
executables so music search works without a system Python or browser:

- [yt-dlp 2026.07.04](https://github.com/yt-dlp/yt-dlp/releases/tag/2026.07.04),
  licensed under The Unlicense; its bundled dependencies retain their own
  licenses as documented by the upstream project.
- [QuickJS-NG 0.15.0](https://github.com/quickjs-ng/quickjs/releases/tag/v0.15.0),
  licensed under the MIT License. It is bundled on Linux, macOS, and Windows
  x86-64.
- [Deno 2.8.1](https://github.com/denoland/deno/releases/tag/v2.8.1), licensed
  under the MIT License. It is bundled only on Windows ARM64, where QuickJS-NG
  does not publish an official ARM64 executable.

Source code and complete license texts are available from the linked upstream
release pages. The pinned versions and SHA-256 checksums used for packaging are
recorded in `.github/workflows/release.yml`.

## Referenced community designs

No upstream source code is redistributed. The built-in HTTP music sources follow
publicly documented community endpoint usage (search plus outer-link/convert_url
playback resolution) from projects such as
[xiaozhi-mcp-music](https://github.com/ABUGG-007/xiaozhi-mcp-music),
[LX Music](https://github.com/lyswhut/lx-music-mobile) and
[MusicFree](https://github.com/maotoumao/MusicFree); the Rust implementations in
`src/provider/netease.rs` and `src/provider/kuwo.rs` are written for this
project. All music sources remain subject to the underlying sites' terms of
service.
