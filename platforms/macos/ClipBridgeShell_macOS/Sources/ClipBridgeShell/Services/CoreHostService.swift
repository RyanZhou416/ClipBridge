import Foundation

@MainActor
final class CoreHostService: ObservableObject {
    enum State: String {
        case notLoaded = "NotLoaded"
        case loading = "Loading"
        case ready = "Ready"
        case degraded = "Degraded"
        case shuttingDown = "ShuttingDown"
    }

    @Published private(set) var state: State = .notLoaded
    @Published private(set) var lastError: String?

    func initialize() async {
        guard state == .notLoaded || state == .degraded else { return }

        state = .loading
        lastError = nil

        do {
            let config = try makeDefaultConfigJSON()
            try CoreBridge.shared.initialize(configJSON: config)
            state = .ready
        } catch {
            state = .degraded
            lastError = String(describing: error)
        }
    }

    func shutdown() async {
        guard state == .ready || state == .degraded else { return }

        state = .shuttingDown
        defer { state = .notLoaded }

        do {
            try CoreBridge.shared.shutdown()
        } catch {
            lastError = String(describing: error)
        }
    }

    private func makeDefaultConfigJSON() throws -> String {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let dataDir = home.appendingPathComponent("Library/Application Support/ClipBridge", isDirectory: true).path
        let cacheDir = home.appendingPathComponent("Library/Caches/ClipBridge", isDirectory: true).path

        let payload: [String: Any] = [
            "device_id": UUID().uuidString.lowercased(),
            "device_name": Host.current().localizedName ?? "macOS",
            "account_uid": "default_user",
            "account_password": "",
            "data_dir": dataDir,
            "cache_dir": cacheDir,
            "app_config": [
                "global_policy": "AllowAll",
                "size_limits": [
                    "soft_text_bytes": 1_048_576
                ]
            ]
        ]

        let data = try JSONSerialization.data(withJSONObject: payload, options: [])
        guard let json = String(data: data, encoding: .utf8) else {
            throw NSError(domain: "ClipBridgeShell", code: 1, userInfo: [NSLocalizedDescriptionKey: "Failed to encode config JSON"])
        }
        return json
    }
}

