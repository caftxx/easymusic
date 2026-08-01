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
