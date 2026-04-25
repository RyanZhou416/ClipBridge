// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "ClipBridgeShell_macOS",
    platforms: [
        .macOS(.v14),
    ],
    products: [
        .executable(name: "ClipBridgeShell", targets: ["ClipBridgeShell"]),
    ],
    targets: [
        .executableTarget(
            name: "ClipBridgeShell",
            path: "Sources/ClipBridgeShell"
        ),
    ]
)

