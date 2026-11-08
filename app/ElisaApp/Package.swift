// swift-tools-version:5.10
// Elisa: the native macOS window for the Elisa coding agent (brand kit "Elisa Main Window v2"). It spawns
// `elisa --serve-jsonl` (elisa/src/jsonl.rs) in the chosen project folder; Shade is drawn by the kit's own renderer
// (elisa-shade.js v2 over elisa-character.js v1, run in JavaScriptCore, SVG drawn by AppKit as vectors).
// Build: swift build -c release   Run: app/ElisaApp/run.sh [--demo] [--cwd DIR]
import PackageDescription

let package = Package(
    name: "ElisaApp",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(
            name: "ElisaApp",
            path: "Sources/ElisaApp",
            // the kit's two OFL fonts (Silkscreen, Chivo Mono; see THIRD_PARTY_NOTICES.md), registered at launch
            // plus the brand kit's app icon and vector wordmarks (designs/), set as the Dock icon and drawn on Welcome
            resources: [.copy("Resources/Fonts"), .copy("Resources/Brand")],
            linkerSettings: [.linkedFramework("AppKit"), .linkedFramework("JavaScriptCore")]
        )
    ]
)
