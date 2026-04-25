import Foundation
import Darwin

enum CoreBridgeError: Error, LocalizedError {
    case libraryNotFound([String])
    case dlopenFailed(String)
    case symbolMissing(String)
    case invalidUTF8
    case invalidEnvelope(String)
    case apiError(code: String, message: String)
    case invalidHandle(UInt)

    var errorDescription: String? {
        switch self {
        case .libraryNotFound(let candidates):
            return "Core dylib not found. Tried: \(candidates.joined(separator: ", "))"
        case .dlopenFailed(let msg):
            return "dlopen failed: \(msg)"
        case .symbolMissing(let name):
            return "Required symbol missing: \(name)"
        case .invalidUTF8:
            return "FFI returned invalid UTF-8 string."
        case .invalidEnvelope(let raw):
            return "Invalid JSON envelope: \(raw)"
        case .apiError(let code, let message):
            return "Core API error (\(code)): \(message)"
        case .invalidHandle(let value):
            return "Invalid handle value returned from core: \(value)"
        }
    }
}

private struct ErrorBody: Decodable {
    let code: String
    let message: String
}

struct CoreFFIVersion {
    let major: UInt32
    let minor: UInt32
}

private typealias CbOnEventFn = @convention(c) (_ json: UnsafePointer<CChar>?, _ userData: UnsafeMutableRawPointer?) -> Void
private typealias CbInitFn = @convention(c) (_ cfgJSON: UnsafePointer<CChar>?, _ onEvent: CbOnEventFn?, _ userData: UnsafeMutableRawPointer?) -> UnsafePointer<CChar>?
private typealias CbGetStatusFn = @convention(c) (_ handle: UnsafeMutableRawPointer?) -> UnsafePointer<CChar>?
private typealias CbShutdownFn = @convention(c) (_ handle: UnsafeMutableRawPointer?) -> UnsafePointer<CChar>?
private typealias CbFreeStringFn = @convention(c) (_ ptr: UnsafePointer<CChar>?) -> Void
private typealias CbGetFFIVersionFn = @convention(c) (_ major: UnsafeMutablePointer<UInt32>?, _ minor: UnsafeMutablePointer<UInt32>?) -> Void

@MainActor
final class CoreBridge {
    static let shared = CoreBridge()

    private var dylibHandle: UnsafeMutableRawPointer?
    private var coreHandle: UnsafeMutableRawPointer?
    private var eventUserData: UnsafeMutableRawPointer?

    private var cbInit: CbInitFn?
    private var cbGetStatus: CbGetStatusFn?
    private var cbShutdown: CbShutdownFn?
    private var cbFreeString: CbFreeStringFn?
    private var cbGetFFIVersion: CbGetFFIVersionFn?

    private init() {}

    deinit {
        if let h = dylibHandle {
            dlclose(h)
        }
    }

    var isInitialized: Bool {
        coreHandle != nil
    }

    func initialize(configJSON: String, onEvent: @escaping (String) -> Void) throws {
        if !isSymbolsLoaded {
            try loadSymbols()
        }

        let eventHandler = Unmanaged.passRetained(EventHandlerBox(onEvent)).toOpaque()

        let eventThunk: CbOnEventFn = { rawJSON, userData in
            guard let rawJSON, let userData else { return }
            let box = Unmanaged<EventHandlerBox>.fromOpaque(userData).takeUnretainedValue()
            box.handle(jsonPtr: rawJSON)
        }

        let envelope = try configJSON.withCString { cstr -> String in
            guard let cbInit else { throw CoreBridgeError.symbolMissing("cb_init") }
            let outPtr = cbInit(cstr, eventThunk, eventHandler)
            return try consumeCString(outPtr)
        }

        let parsed = try parseEnvelope(envelope)
        guard parsed.ok else {
            let err = parsed.error ?? ErrorBody(code: "UNKNOWN", message: envelope)
            Unmanaged<EventHandlerBox>.fromOpaque(eventHandler).release()
            throw CoreBridgeError.apiError(code: err.code, message: err.message)
        }
        guard let handleValue = parseHandle(envelope) else {
            Unmanaged<EventHandlerBox>.fromOpaque(eventHandler).release()
            throw CoreBridgeError.invalidEnvelope(envelope)
        }
        guard let pointer = UnsafeMutableRawPointer(bitPattern: handleValue) else {
            Unmanaged<EventHandlerBox>.fromOpaque(eventHandler).release()
            throw CoreBridgeError.invalidHandle(handleValue)
        }

        coreHandle = pointer
        eventUserData = eventHandler
    }

    func getStatusJSON() throws -> String {
        guard let coreHandle else { throw CoreBridgeError.invalidEnvelope("Core not initialized") }
        guard let cbGetStatus else { throw CoreBridgeError.symbolMissing("cb_get_status") }
        let envelope = try consumeCString(cbGetStatus(coreHandle))
        let parsed = try parseEnvelope(envelope)
        if !parsed.ok {
            let err = parsed.error ?? ErrorBody(code: "UNKNOWN", message: envelope)
            throw CoreBridgeError.apiError(code: err.code, message: err.message)
        }
        return envelope
    }

    func shutdown() throws {
        guard let handle = coreHandle else { return }
        guard let cbShutdown else { throw CoreBridgeError.symbolMissing("cb_shutdown") }
        let envelope = try consumeCString(cbShutdown(handle))
        let parsed = try parseEnvelope(envelope)
        if !parsed.ok {
            let err = parsed.error ?? ErrorBody(code: "UNKNOWN", message: envelope)
            throw CoreBridgeError.apiError(code: err.code, message: err.message)
        }
        coreHandle = nil
        releaseEventHandlerIfNeeded()
    }

    func getFFIVersion() throws -> CoreFFIVersion {
        if !isSymbolsLoaded {
            try loadSymbols()
        }
        guard let cbGetFFIVersion else { throw CoreBridgeError.symbolMissing("cb_get_ffi_version") }
        var major: UInt32 = 0
        var minor: UInt32 = 0
        cbGetFFIVersion(&major, &minor)
        return CoreFFIVersion(major: major, minor: minor)
    }

    private var isSymbolsLoaded: Bool {
        cbInit != nil && cbGetStatus != nil && cbShutdown != nil && cbFreeString != nil && cbGetFFIVersion != nil
    }

    private func loadSymbols() throws {
        let candidates = dylibCandidates()
        var lastError = "unknown"
        for path in candidates {
            let h = dlopen(path, RTLD_NOW | RTLD_LOCAL)
            if let h {
                dylibHandle = h
                do {
                    cbInit = try loadSymbol(handle: h, name: "cb_init", as: CbInitFn.self)
                    cbGetStatus = try loadSymbol(handle: h, name: "cb_get_status", as: CbGetStatusFn.self)
                    cbShutdown = try loadSymbol(handle: h, name: "cb_shutdown", as: CbShutdownFn.self)
                    cbFreeString = try loadSymbol(handle: h, name: "cb_free_string", as: CbFreeStringFn.self)
                    cbGetFFIVersion = try loadSymbol(handle: h, name: "cb_get_ffi_version", as: CbGetFFIVersionFn.self)
                    return
                } catch {
                    dlclose(h)
                    dylibHandle = nil
                    throw error
                }
            } else if let cErr = dlerror() {
                lastError = String(cString: cErr)
            }
        }
        if candidates.isEmpty {
            throw CoreBridgeError.libraryNotFound([])
        }
        if lastError != "unknown" {
            throw CoreBridgeError.dlopenFailed(lastError)
        }
        throw CoreBridgeError.libraryNotFound(candidates)
    }

    private func dylibCandidates() -> [String] {
        var out: [String] = []
        let fm = FileManager.default

        if let env = ProcessInfo.processInfo.environment["CLIPBRIDGE_CORE_LIB"], !env.isEmpty {
            out.append(env)
        }

        let cwd = fm.currentDirectoryPath
        out.append((cwd as NSString).appendingPathComponent("../Native/libcore_ffi_macos.dylib"))
        out.append((cwd as NSString).appendingPathComponent("Native/libcore_ffi_macos.dylib"))

        if let execURL = Bundle.main.executableURL?.deletingLastPathComponent() {
            out.append(execURL.appendingPathComponent("libcore_ffi_macos.dylib").path)
            out.append(execURL.appendingPathComponent("../Frameworks/libcore_ffi_macos.dylib").path)
        }
        if let fwURL = Bundle.main.privateFrameworksURL {
            out.append(fwURL.appendingPathComponent("libcore_ffi_macos.dylib").path)
        }

        var dedup: [String] = []
        for p in out where !dedup.contains(p) {
            dedup.append(p)
        }
        return dedup
    }

    private func loadSymbol<T>(handle: UnsafeMutableRawPointer, name: String, as _: T.Type) throws -> T {
        guard let sym = dlsym(handle, name) else {
            throw CoreBridgeError.symbolMissing(name)
        }
        return unsafeBitCast(sym, to: T.self)
    }

    private func consumeCString(_ ptr: UnsafePointer<CChar>?) throws -> String {
        guard let ptr else { throw CoreBridgeError.invalidUTF8 }
        defer { cbFreeString?(ptr) }
        guard let s = String(validatingUTF8: ptr) else {
            throw CoreBridgeError.invalidUTF8
        }
        return s
    }

    private func parseEnvelope(_ raw: String) throws -> (ok: Bool, error: ErrorBody?) {
        guard let data = raw.data(using: .utf8),
              let obj = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw CoreBridgeError.invalidEnvelope(raw)
        }
        let ok = obj["ok"] as? Bool ?? false

        var errorBody: ErrorBody?
        if let errObj = obj["error"] as? [String: Any] {
            let code = errObj["code"] as? String ?? "UNKNOWN"
            let message = errObj["message"] as? String ?? raw
            errorBody = ErrorBody(code: code, message: message)
        }
        return (ok, errorBody)
    }

    private func parseHandle(_ raw: String) -> UInt? {
        guard let data = raw.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let dataObj = obj["data"] as? [String: Any],
              let rawHandle = dataObj["handle"] else {
            return nil
        }

        if let n = rawHandle as? NSNumber {
            return n.uintValue
        }
        if let s = rawHandle as? String, let v = UInt(s) {
            return v
        }
        return nil
    }

    private func releaseEventHandlerIfNeeded() {
        guard let eventUserData else { return }
        Unmanaged<EventHandlerBox>.fromOpaque(eventUserData).release()
        self.eventUserData = nil
    }
}

private final class EventHandlerBox {
    private let onEvent: (String) -> Void

    init(_ onEvent: @escaping (String) -> Void) {
        self.onEvent = onEvent
    }

    func handle(jsonPtr: UnsafePointer<CChar>) {
        if let s = String(validatingUTF8: jsonPtr) {
            onEvent(s)
        }
    }
}
