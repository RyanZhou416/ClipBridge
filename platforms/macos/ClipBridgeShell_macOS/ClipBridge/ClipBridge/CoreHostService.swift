import Foundation
import Combine
import Security

@MainActor
final class CoreHostService: ObservableObject {
    private struct AccountCredentials {
        let uid: String
        let password: String
    }

    private enum ConfigError: LocalizedError {
        case unsupportedFFIABI(found: String, expectedMajor: UInt32)
        case missingAccountUID
        case missingAccountPassword

        var errorDescription: String? {
            switch self {
            case .unsupportedFFIABI(let found, let expectedMajor):
                return "Unsupported FFI ABI version \(found). Expected major \(expectedMajor)."
            case .missingAccountUID:
                return "Missing account UID. Set `clipbridge.account_uid` in UserDefaults or CLIPBRIDGE_ACCOUNT_UID."
            case .missingAccountPassword:
                return "Missing account password in Keychain/UserDefaults/environment."
            }
        }
    }

    enum State: String {
        case notLoaded = "NotLoaded"
        case loading = "Loading"
        case ready = "Ready"
        case degraded = "Degraded"
        case shuttingDown = "ShuttingDown"
    }

    @Published private(set) var state: State = .notLoaded
    @Published private(set) var lastError: String?
    @Published private(set) var ffiABIVersion: String?
    @Published private(set) var lastStatusJSON: String?
    @Published private(set) var lastEventJSON: String?
    @Published private(set) var lastEventType: String?
    @Published private(set) var lastEventPayloadJSON: String?
    @Published private(set) var lastEventParseError: String?

    private let expectedFFIABIMajor: UInt32 = 1

    var canQuery: Bool { state == .ready }
    var canShutdown: Bool { state == .ready || state == .degraded }

    func initialize() async {
        guard state == .notLoaded || state == .degraded else { return }
        state = .loading
        lastError = nil

        do {
            let abi = try CoreBridge.shared.getFFIVersion()
            ffiABIVersion = "\(abi.major).\(abi.minor)"
            guard abi.major == expectedFFIABIMajor else {
                throw ConfigError.unsupportedFFIABI(found: "\(abi.major).\(abi.minor)", expectedMajor: expectedFFIABIMajor)
            }

            let config = try makeConfigJSON()
            try CoreBridge.shared.initialize(configJSON: config) { [weak self] event in
                Task { @MainActor [weak self] in
                    self?.consumeEvent(event)
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
        do {
            try CoreBridge.shared.shutdown()
            state = .notLoaded
            lastError = nil
        } catch {
            state = .degraded
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

        let account = try readAccountCredentials()

        let payload: [String: Any] = [
            "device_id": readOrCreateDeviceID(),
            "device_name": Host.current().localizedName ?? "macOS",
            "account_uid": account.uid,
            "account_password": account.password,
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

    private func readAccountCredentials() throws -> AccountCredentials {
        let defaults = UserDefaults.standard
        let env = ProcessInfo.processInfo.environment

        let uid = (
            defaults.string(forKey: "clipbridge.account_uid")
            ?? env["CLIPBRIDGE_ACCOUNT_UID"]
        )?.trimmingCharacters(in: .whitespacesAndNewlines)

        guard let uid, !uid.isEmpty else {
            throw ConfigError.missingAccountUID
        }

        if let fromKeychain = readPasswordFromKeychain(accountUID: uid) {
            return AccountCredentials(uid: uid, password: fromKeychain)
        }

        if let fromDefaults = defaults.string(forKey: "clipbridge.account_password"), !fromDefaults.isEmpty {
            return AccountCredentials(uid: uid, password: fromDefaults)
        }

        if let fromEnv = env["CLIPBRIDGE_ACCOUNT_PASSWORD"], !fromEnv.isEmpty {
            return AccountCredentials(uid: uid, password: fromEnv)
        }

        throw ConfigError.missingAccountPassword
    }

    private func readPasswordFromKeychain(accountUID: String) -> String? {
        let service = "ClipBridge"
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: accountUID,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne
        ]

        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        guard status == errSecSuccess,
              let data = item as? Data,
              let password = String(data: data, encoding: .utf8),
              !password.isEmpty else {
            return nil
        }
        return password
    }

    private func consumeEvent(_ raw: String) {
        lastEventJSON = raw
        lastEventType = nil
        lastEventPayloadJSON = nil
        lastEventParseError = nil

        guard let data = raw.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data, options: []),
              let root = obj as? [String: Any] else {
            lastEventParseError = "Invalid event JSON."
            return
        }

        guard let eventType = root["type"] as? String, !eventType.isEmpty else {
            lastEventParseError = "Event is missing required 'type'."
            return
        }

        lastEventType = eventType

        guard let payload = root["payload"] else {
            return
        }

        if JSONSerialization.isValidJSONObject(payload),
           let payloadData = try? JSONSerialization.data(withJSONObject: payload, options: []),
           let payloadJSON = String(data: payloadData, encoding: .utf8) {
            lastEventPayloadJSON = payloadJSON
            return
        }

        if let payloadString = payload as? String {
            lastEventPayloadJSON = payloadString
            return
        }

        // 对于 number/bool/null 这类 JSON 原始值，统一降级为文本展示。
        lastEventPayloadJSON = String(describing: payload)
    }
}
