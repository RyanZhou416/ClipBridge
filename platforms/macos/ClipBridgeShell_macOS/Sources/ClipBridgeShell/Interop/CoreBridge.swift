import Foundation

enum CoreBridgeError: Error {
    case notImplemented
}

@MainActor
final class CoreBridge {
    static let shared = CoreBridge()

    private init() {}

    func initialize(configJSON: String) throws {
        _ = configJSON
        throw CoreBridgeError.notImplemented
    }

    func shutdown() throws {
        throw CoreBridgeError.notImplemented
    }
}
