import Containerization
import ContainerizationOCI
import Foundation
import Synchronization

struct Share: Decodable, Sendable {
    let source: String
    let destination: String
    let readOnly: Bool
}

struct Request: Decodable, Sendable {
    let operation: String
    var store: String?
    var kernel: String?
    var initfs: String?
    var initReference: String?
    var image: String?
    var layout: String?
    var rootfs: String?
    var memoryMb: UInt64?
    var rootfsSizeMb: UInt64?
    var cpus: Int?
    var network: Bool?
    var mounts: [Share]?
    var arguments: [String]?
    var environment: [String]?
    var cwd: String?
    var uid: UInt32?
    var gid: UInt32?
    var input: [UInt8]?
    var timeoutSeconds: Int64?
    var diagnostics: Bool?
}

struct HostError: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

final class Output: Writer {
    private let bytes = Mutex(Data())
    private let diagnostics: Bool
    init(diagnostics: Bool = false) { self.diagnostics = diagnostics }
    func write(_ data: Data) throws {
        bytes.withLock { $0.append(data) }
        if diagnostics { try FileHandle.standardError.write(contentsOf: data) }
    }
    func close() throws {}
    func snapshot() -> [UInt8] { bytes.withLock { Array($0) } }
}

struct Input: ReaderStream {
    let bytes: Data
    func stream() -> AsyncStream<Data> {
        AsyncStream { continuation in
            continuation.yield(bytes)
            continuation.finish()
        }
    }
}

struct DiagnosticOutput: Writer {
    func write(_ data: Data) throws { try FileHandle.standardError.write(contentsOf: data) }
    func close() throws {}
}

actor Host {
    private var manager: ContainerManager?
    private var container: LinuxContainer?
    private var closed = false
    private var containerID: String?

    func shutdown() async {
        closed = true
        try? await stop()
    }

    func stop() async throws {
        if let container {
            try await container.stop()
            self.container = nil
        }
        if let id = containerID, var manager {
            try manager.delete(id)
            self.manager = nil
            containerID = nil
        }
    }

    private func store(_ request: Request) throws -> ImageStore {
        guard let path = request.store, path.hasPrefix("/") else {
            throw HostError("Set an absolute image store path.")
        }
        return try ImageStore(path: URL(filePath: path))
    }

    private func image(_ store: ImageStore, reference: String) async throws -> Containerization.Image {
        if let image = try await store.getOrNil(reference: reference) { return image }
        return try await store.pull(reference: reference, platform: .current)
    }

    func handle(_ request: Request) async throws -> Data {
        guard !closed else { throw HostError("The controller pipe is closed.") }
        switch request.operation {
        case "hello":
            return try encode(["protocolVersion": 1, "sdkVersion": "0.57.0"])
        case "images":
            let images = try await store(request).list()
            return try encode(["images": images.map(\.reference)])
        case "pull":
            guard let image = request.image else { throw HostError("Set an image reference.") }
            let result = try await self.image(store(request), reference: image)
            return try encode(["reference": result.reference, "digest": result.descriptor.digest])
        case "remove":
            guard let image = request.image else { throw HostError("Set an image reference.") }
            try await store(request).delete(reference: image, performCleanup: true)
            return try encode(["removed": true])
        case "load":
            guard let layout = request.layout else { throw HostError("Set an OCI layout directory.") }
            let images = try await store(request).load(from: URL(filePath: layout))
            return try encode(["images": images.map(\.reference)])
        case "unpack":
            guard let image = request.image, let rootfs = request.rootfs else {
                throw HostError("Set the image reference and root filesystem path.")
            }
            let source = try await self.image(store(request), reference: image)
            let temporary = URL(filePath: rootfs + ".partial")
            defer { try? FileManager.default.removeItem(at: temporary) }
            _ = try await EXT4Unpacker(capacityInBytes: (request.rootfsSizeMb ?? 16384) * 1024 * 1024)
                .unpack(source, for: .current, at: temporary)
            try FileManager.default.moveItem(at: temporary, to: URL(filePath: rootfs))
            return try encode(["rootfs": rootfs])
        case "start":
            try await start(request)
            return try encode(["started": true, "id": containerID ?? ""])
        case "exec":
            return try await execute(request)
        case "stop":
            try await stop()
            return try encode(["stopped": true])
        default:
            throw HostError("Unknown helper operation: \(request.operation)")
        }
    }

    private func start(_ request: Request) async throws {
        guard container == nil else { throw HostError("A worker is already active.") }
        guard let kernel = request.kernel, let image = request.image else {
            throw HostError("Set the kernel path and image reference.")
        }
        let imageStore = try store(request)
        let network: Network? = request.network == true ? try VmnetNetwork() : nil
        let bootKernel = Kernel(path: URL(filePath: kernel), platform: .linuxArm)
        var manager: ContainerManager
        if let initfs = request.initfs {
            manager = try ContainerManager(
                kernel: bootKernel,
                initfs: .block(format: "ext4", source: initfs, destination: "/", options: ["ro"]),
                imageStore: imageStore, network: network
            )
        } else {
            manager = try await ContainerManager(
                kernel: bootKernel,
                initfsReference: request.initReference ?? "ghcr.io/apple/containerization/vminit@sha256:c30d5a14c9c25762005363c1736831608806ee8c2a9d8de70dd5439d4812e656",
                imageStore: imageStore, network: network
            )
        }
        let source = try await self.image(imageStore, reference: image)
        guard !closed else { throw HostError("The controller pipe is closed.") }
        let id = "noctum-\(UUID().uuidString.lowercased())"
        let cpus = request.cpus ?? 2
        let memory = (request.memoryMb ?? 2048) * 1024 * 1024
        let resources = VMResources(cpus: cpus, memoryInBytes: memory + VMResources.guestMemoryOverhead)
        let configure: (inout LinuxContainer.Configuration) throws -> Void = { config in
            config.cpus = cpus
            config.memoryInBytes = memory
            config.process.arguments = request.arguments ?? ["/bin/sleep", "infinity"]
            config.process.environmentVariables = Self.environment(
                config.process.environmentVariables,
                (request.environment ?? []) + ["NOCTUM_WORKER_SESSION=\(id)"]
            )
            config.process.workingDirectory = request.cwd ?? "/"
            config.process.stdout = DiagnosticOutput()
            config.process.stderr = DiagnosticOutput()
            config.mounts += (request.mounts ?? []).map {
                Mount.share(source: $0.source, destination: $0.destination, options: $0.readOnly ? ["ro"] : [])
            }
        }
        let worker: LinuxContainer
        if let rootfs = request.rootfs {
            let directory = imageStore.path.appendingPathComponent("containers/\(id)")
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            worker = try await manager.create(id, image: source, rootfs: .block(format: "ext4", source: rootfs, destination: "/"), networking: request.network == true, vm: resources, configuration: configure)
        } else {
            worker = try await manager.create(id, image: source, rootfsSizeInBytes: (request.rootfsSizeMb ?? 4096) * 1024 * 1024, networking: request.network == true, vm: resources, configuration: configure)
        }
        if closed {
            try? manager.delete(id)
            throw HostError("The controller pipe is closed.")
        }
        self.manager = manager
        self.containerID = id
        self.container = worker
        try await worker.create()
        guard !closed else { throw HostError("The controller pipe is closed.") }
        try await worker.start()
    }

    private func execute(_ request: Request) async throws -> Data {
        guard let container, let arguments = request.arguments, !arguments.isEmpty else {
            throw HostError("Start a worker and set the command arguments.")
        }
        let output = Output(diagnostics: request.diagnostics ?? false)
        let errors = Output(diagnostics: request.diagnostics ?? false)
        var configuration = LinuxProcessConfiguration()
        configuration.arguments = arguments
        configuration.environmentVariables = Self.environment(container.config.process.environmentVariables, request.environment ?? [])
        configuration.workingDirectory = request.cwd ?? "/"
        configuration.user = .init(uid: request.uid ?? 0, gid: request.gid ?? 0)
        configuration.stdin = Input(bytes: Data(request.input ?? []))
        configuration.stdout = output
        configuration.stderr = errors
        let process = try await container.exec(UUID().uuidString, configuration: configuration)
        try await process.start()
        let result = try await process.wait(timeoutInSeconds: request.timeoutSeconds ?? 600)
        try await process.delete()
        return try encode(["code": result.exitCode, "stdout": output.snapshot(), "stderr": errors.snapshot()])
    }

    private static func environment(_ base: [String], _ overrides: [String]) -> [String] {
        var values: [String: String] = [:]
        for item in base + overrides {
            let parts = item.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
            if parts.count == 2 { values[String(parts[0])] = String(parts[1]) }
        }
        return values.keys.sorted().map { "\($0)=\(values[$0]!)" }
    }
}

func encode(_ value: [String: Any]) throws -> Data {
    try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
}

func reply(_ value: Data?, error: String? = nil) throws {
    let payload: [String: Any] = [
        "value": try value.map { try JSONSerialization.jsonObject(with: $0) } ?? NSNull(),
        "error": error as Any? ?? NSNull()
    ]
    var bytes = try JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])
    bytes.append(10)
    try FileHandle.standardOutput.write(contentsOf: bytes)
}

@main
struct NoctumHost {
    static func main() async throws {
        signal(SIGPIPE, SIG_IGN)
        let host = Host()
        let lines = AsyncThrowingStream<String, Error> { continuation in
            Task.detached {
                do {
                    for try await line in FileHandle.standardInput.bytes.lines {
                        continuation.yield(line)
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
                await host.shutdown()
                exit(0)
            }
        }
        do {
            for try await line in lines {
                do {
                    let request = try JSONDecoder().decode(Request.self, from: Data(line.utf8))
                    try reply(try await host.handle(request))
                } catch {
                    try? await host.stop()
                    try reply(nil, error: String(describing: error))
                }
            }
        } catch {
            await host.shutdown()
            throw error
        }
        await host.shutdown()
    }
}
