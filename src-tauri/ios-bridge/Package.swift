// swift-tools-version:5.5
// App-internal Swift side of ios_bridge.rs, linked into the Rust library by
// build.rs (same mechanism official Tauri plugins use; see build.rs for why
// this can't live directly in the Xcode app target).

import PackageDescription

let package = Package(
  name: "reflectodoro-bridge",
  platforms: [
    .macOS(.v10_13),
    .iOS(.v14),
  ],
  products: [
    .library(
      name: "reflectodoro-bridge",
      type: .static,
      targets: ["reflectodoro-bridge"])
  ],
  dependencies: [
    .package(name: "Tauri", path: ".tauri/tauri-api")
  ],
  targets: [
    .target(
      name: "reflectodoro-bridge",
      dependencies: [
        .byName(name: "Tauri")
      ],
      path: "Sources")
  ]
)
