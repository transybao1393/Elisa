// swift-tools-version:5.10
// Elisa companion: a small floating window that shows Shade (the brand kit's own renderer in a WKWebView) and follows
// Elisa's live state over a Unix socket. Build: swift build -c release   Run: app/ElisaCompanion/run.sh
import PackageDescription

let package = Package(
    name: "ElisaCompanion",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(
            name: "ElisaCompanion",
            path: "Sources/ElisaCompanion",
            resources: [.copy("Resources/companion.html")],
            linkerSettings: [.linkedFramework("AppKit"), .linkedFramework("WebKit")]
        )
    ]
)
