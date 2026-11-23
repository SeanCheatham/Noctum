// swift-tools-version: 6.4
import PackageDescription

let package = Package(
    name: "NoctumHost",
    platforms: [.macOS(.v26)],
    products: [.executable(name: "noctum-host", targets: ["NoctumHost"])],
    dependencies: [
        .package(url: "https://github.com/apple/containerization.git", exact: "0.57.0")
    ],
    targets: [
        .executableTarget(
            name: "NoctumHost",
            dependencies: [
                .product(name: "Containerization", package: "containerization"),
                .product(name: "ContainerizationOCI", package: "containerization"),
                .product(name: "ContainerizationEXT4", package: "containerization")
            ]
        )
    ]
)
