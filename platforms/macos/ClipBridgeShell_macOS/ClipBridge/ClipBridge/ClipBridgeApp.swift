//
//  ClipBridgeApp.swift
//  ClipBridge
//
//  Created by 周熙然 on 2026-04-25.
//

import SwiftUI

@main
struct ClipBridgeApp: App {
    @StateObject private var coreHost = CoreHostService()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(coreHost)
        }
    }
}
