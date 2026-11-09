# Building a release

A release is `Elisa-<version>.dmg`: a disk image holding `Elisa.app` (the SwiftUI window with the Rust core inside)
and a link to `/Applications`. `scripts/package.sh` builds it; `.github/workflows/release.yml` runs the script when a
version tag is pushed and attaches the .dmg to a GitHub Release.

## What is inside Elisa.app

| Path | What |
|---|---|
| `Contents/MacOS/ElisaApp` | the window (`swift build -c release` in `app/ElisaApp`) |
| `Contents/MacOS/elisa` | the core (`cargo build --release` in `elisa`) |
| `Contents/Resources/ElisaApp_ElisaApp.bundle` | the fonts (Silkscreen, Chivo Mono) and brand files |
| `Contents/Resources/design` | the brand kit (`elisa/design`) the window draws Shade from |
| `Contents/Resources/Elisa.icns` | the app icon |
| `Contents/Info.plist` | `app/ElisaApp/Info.plist` plus the bundle keys; `LSMinimumSystemVersion` 14.0 |

The window looks for the core in this order: `$ELISA_BIN`, then `Contents/MacOS/elisa` inside the app bundle, then
`elisa/target/release/elisa` and `elisa/target/debug/elisa` in the repository (`Link.swift`). The brand kit is found
the same way: `$ELISA_KIT_DIR`, then `Contents/Resources/design`, then the repository (`Kit.swift`). So a built app
uses its own copies, and `run.sh` / `dev.sh` keep using the repository.

The bundled app has its own bundle identifier (`io.github.transybao1393.elisa` by default), so its window settings
are kept apart from those of the app run from source (which uses the `ElisaApp` preferences domain).

## Build locally (unsigned)

Requirements: macOS 14 or later, Xcode with the macOS 15 SDK, a Rust toolchain.

```sh
scripts/package.sh
open dist/Elisa-0.1.0.dmg
```

Without a signing identity the app is ad-hoc signed. It runs on the Mac that built it. On another Mac, Gatekeeper
blocks a double-click; the user can right-click > Open, or run `xattr -d com.apple.quarantine /Applications/Elisa.app`.

Environment variables:

| Variable | Effect |
|---|---|
| `VERSION` | the version in Info.plist and the file name (default: `elisa/Cargo.toml`); a leading `v` is dropped |
| `SIGN_IDENTITY` | sign with this Developer ID identity, hardened runtime, secure timestamp |
| `NOTARY_PROFILE` | notarise the .dmg with this `notarytool` profile and staple the ticket (needs `SIGN_IDENTITY`) |
| `BUNDLE_ID` | `CFBundleIdentifier` (default `io.github.transybao1393.elisa`) |
| `OUT_DIR` | output folder (default `dist/`) |
| `SKIP_BUILD=1` | reuse the existing release builds |

## Sign and notarise

You need an Apple Developer account and a **Developer ID Application** certificate in your login keychain.

1. Find the identity name:

   ```sh
   security find-identity -v -p codesigning
   # "Developer ID Application: Your Name (TEAMID)"
   ```

2. Store notarisation credentials once (an app-specific password from appleid.apple.com):

   ```sh
   xcrun notarytool store-credentials elisa-notary --apple-id you@example.com --team-id TEAMID --password xxxx-xxxx-xxxx-xxxx
   ```

3. Build, sign and notarise:

   ```sh
   SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" NOTARY_PROFILE=elisa-notary VERSION=0.1.0 scripts/package.sh
   ```

The script signs from the inside out (the resource bundle, the core, then the app with `scripts/entitlements.plist`),
checks the signature, builds the .dmg, signs it, submits it to Apple's notary service, waits, and staples the ticket.
The only entitlement is `com.apple.security.device.audio-input`: the hardened runtime blocks the microphone without it,
and voice input needs it. The app is not App Sandboxed, because the core reads project folders and runs the commands
you approve (see [sandbox.md](sandbox.md) for how those are fenced in).

Check the result:

```sh
spctl --assess --type open --context context:primary-signature --verbose=2 dist/Elisa-0.1.0.dmg
xcrun stapler validate dist/Elisa-0.1.0.dmg
```

## Release from GitHub

Push a version tag:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

The `Release` workflow builds on `macos-14`, runs `scripts/package.sh`, and uploads the .dmg and its SHA-256 to a
GitHub Release named after the tag (creating it with generated notes, or adding to it if it exists). It can also be
started by hand (Actions > Release > Run workflow) for an existing tag.

Signing and notarisation run only when these repository secrets exist; without them the release is ad-hoc signed:

| Secret | What |
|---|---|
| `MACOS_CERT_P12_BASE64` | the Developer ID Application certificate and key, exported as .p12, then `base64 -i cert.p12` |
| `MACOS_CERT_PASSWORD` | the .p12 password |
| `MACOS_SIGN_IDENTITY` | the identity name, e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD` | notarisation (needs the three signing secrets too) |

The certificate goes into a temporary keychain that is deleted at the end of the job.
