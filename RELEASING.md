# Releasing

Every version that ships is a GitHub release. Pushing a version tag is all it takes:
CI builds the app and the installer, then publishes both with that version's changelog notes.

1. **Bump the version** in `Cargo.toml` (`version = "1.1.0"`). The app, the exe details and the installer all read it from there.
2. **Update `CHANGELOG.md`**: add a `## [1.1.0] - YYYY-MM-DD` section at the top describing what changed. It becomes the release notes.
3. **Check locally**: `cargo fmt --check`, `cargo clippy --release -- -D warnings`, `cargo build --release`.
4. **Commit and push** to `main`.
5. **Tag and push the tag**:
   ```bash
   git tag v1.1.0
   git push origin v1.1.0
   ```
6. CI checks that the tag matches `Cargo.toml`, builds everything, and publishes **Music Notch v1.1.0** with:
   - `Music-Notch-Setup-1.1.0.exe`, the installer (upgrades an existing install in place)
   - `Music-Notch-1.1.0-portable.exe`, the single portable file

Tags with a suffix, like `v1.2.0-beta.1`, are published as pre-releases.

Version numbers follow [Semantic Versioning](https://semver.org/): fixes bump the last number (1.0.1),
new features the middle one (1.1.0), and breaking changes the first one (2.0.0).
