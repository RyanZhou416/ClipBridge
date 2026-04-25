import Foundation
import Combine

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
    @Published private(set) var lastStatusJSON: String?
    @Published private(set) var lastEventJSON: String?

    var canQuery: Bool { state == .ready }
    var canShutdown: Bool { state == .ready || state == .degraded }

    func initialize() async {
        guard state == .notLoaded || state == .degraded else { return }
        state = .loading
        lastError = nil

        do {
            let config = try makeConfigJSON()
            try CoreBridge.shared.initialize(configJSON: config) { [weak self] event in
                Task { @MainActor [weak self] in
                    self?.lastEventJSON = event
                }
            }
            state = .ready
        } catch {
            state = .degraded
            lastError = String(describing: error)
        }
    }

    func refreshStatus() async {
        guard canQuery else { return }
        do {
            lastStatusJSON = try CoreBridge.shared.getStatusJSON()
            lastError = nil
        } catch {
            lastError = String(describing: error)
        }
    }

    func shutdown() async {
        guard canShutdown else { return }
        state = .shuttingDown
        defer { state = .notLoaded }
        do {
            try CoreBridge.shared.shutdown()
            lastError = nil
        } catch {
            lastError = String(describing: error)
        }
    }

    private func makeConfigJSON() throws -> String {
        let fm = FileManager.default
        let home = fm.homeDirectoryForCurrentUser
        let dataDir = home.appendingPathComponent("Library/Application Support/ClipBridge", isDirectory: true).path
        let cacheDir = home.appendingPathComponent("Library/Caches/ClipBridge", isDirectory: true).path

        try ensureDirectory(dataDir)
        try ensureDirectory(cacheDir)

        let payload: [String: Any] = [
            "device_id": readOrCreateDeviceID(),
            "device_name": Host.current().localizedName ?? "macOS",
            "account_uid": "default_user",
            "account_password": "",
            "data_dir": dataDir,
            "cache_dir": cacheDir,
            "app_config": [
                "global_policy": "AllowAll",
                "size_limits": [
                    "soft_text_bytes": 1_048_576,
                    "soft_image_bytes": 5_242_880,
                    "soft_file_total_bytes": 20_971_520
                ]
            ]
        ]

        let data = try JSONSerialization.data(withJSONObject: payload, options: [])
        guard let json = String(data: data, encoding: .utf8) else {
            throw NSError(domain: "ClipBridge", code: 1, userInfo: [NSLocalizedDescriptionKey: "Failed to encode Core config JSON"])
        }
        return json
    }

    private func ensureDirectory(_ path: String) throws {
        try FileManager.default.createDirectory(atPath: path, withIntermediateDirectories: true)
    }

    private func readOrCreateDeviceID() -> String {
        let defaults = UserDefaults.standard
        let key = "clipbridge.device_id"
        if let existing = defaults.string(forKey: key), !existing.isEmpty {
            return existing
        }
        let newID = UUID().uuidString.lowercased()
        defaults.set(newID, forKey: key)
        return newID
    }
}
