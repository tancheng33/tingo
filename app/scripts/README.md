# macOS release signing

The normal Tauri config uses ad-hoc signing for local development. Official builds use `tauri.release.conf.json` and the Developer ID Application identity installed in the login keychain. `build.rs` signs the embedded recorder with the same identity and hardened runtime.

From `app/`:

```sh
npm run release:mac
```

Once per Mac, store notarization credentials interactively (never commit a password or private key):

```sh
xcrun notarytool store-credentials tingo-notary --apple-id YOUR_APPLE_ID --team-id 9YJL5M4BY8
```

Enter an Apple Account app-specific password at the secure prompt. Then notarize the generated DMG:

```sh
bash scripts/notarize-macos.sh src-tauri/target/release/bundle/dmg/Tingo_0.5.0_aarch64.dmg
```

Use the actual version's filename. The script waits for acceptance, staples the ticket, checks Gatekeeper, and prints the final checksum. Generate release checksums **after** stapling. A signed build alone is not a notarized release. Keep signing assets and credentials outside the repository.

## In-app updates (v0.5.2+)

The desktop shell uses the official Tauri updater. It downloads and verifies a version-bound signature, keeps the package in memory until the user selects **Restart and update**, and checks the engine is idle before installation. Closing the app before installation discards the in-memory package; it can be downloaded again. No updater installation commands are exposed through the local HTTP service.

The private updater key is stored outside this repository at `~/.config/tingo-signing/updater.key` (mode 0600). Back it up securely: existing apps trust its public key in `tauri.conf.json`. Never replace the key on routine releases. `TAURI_SIGNING_PRIVATE_KEY` may override the **file path** and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` the password. Ordinary development builds do not require the key; official builds do.

After `npm run release:mac` and DMG notarization, run:

```sh
python3 scripts/prepare-update.py --notes /path/to/release-notes.md
```

This requires accepted Apple notarization. It staples the app, recreates `Tingo.app.tar.gz` from the stapled app, signs it with `--app-version`, and writes `latest.json` plus checksums.

The same run signs the Windows installer and adds it to `latest.json` as `windows-x86_64`, so build it first with `pnpm release:windows` (same version), or pass the CI artifact with `--windows-installer /path/Tingo_<version>_x64-setup.exe`. The script stops if the installer is missing or named for another version; `--skip-windows` publishes a macOS-only release, which Windows apps treat as no update. The NSIS installer itself is the Windows update package; the app runs it in passive mode and it relaunches Tingo.

Upload the DMG, archive, Windows installer, both `.sig` files, `latest.json` and `checksums.txt` to the same version's **draft** GitHub release, then publish as latest. The app checks `https://github.com/tancheng33/tingo/releases/latest/download/latest.json`; do not mark a release latest before all assets are uploaded.

To validate a finalized artifact without touching the installed app:

```sh
TINGO_UPDATE_TEST_ARCHIVE=/absolute/path/Tingo.app.tar.gz \
TINGO_UPDATE_TEST_MANIFEST=/absolute/path/latest.json \
cargo +1.95.0 test --manifest-path src-tauri/Cargo.toml --lib signed_archive_install_and_tamper_rejection -- --ignored
```

The test rejects altered bytes and a mismatched version, installs into a temporary fixture, and checks both the resulting Apple signature and stapled ticket.

For the Windows entry (runs on macOS; it verifies but does not install):

```sh
TINGO_UPDATE_TEST_INSTALLER=/absolute/path/Tingo_<version>_x64-setup.exe \
TINGO_UPDATE_TEST_MANIFEST=/absolute/path/latest.json \
cargo +1.95.0 test --manifest-path src-tauri/Cargo.toml --lib signed_windows_installer_verifies_and_rejects_tampering -- --ignored
``` A production update refuses to install while any engine task or directory switch is active. Updates to a protected Applications folder may require macOS administrator authorization; cancellations are reported and the app stays open.
