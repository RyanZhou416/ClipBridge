//
//  ContentView.swift
//  ClipBridge
//
//  Created by 周熙然 on 2026-04-25.
//

import SwiftUI

struct ContentView: View {
    @EnvironmentObject var coreHost: CoreHostService

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("ClipBridge macOS Shell")
                .font(.title2)
                .bold()

            HStack(spacing: 8) {
                Text("Core State:")
                Text(coreHost.state.rawValue)
                    .fontWeight(.semibold)
            }

            if let status = coreHost.lastStatusJSON, !status.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Status JSON:")
                        .font(.headline)
                    ScrollView {
                        Text(status)
                            .font(.system(.footnote, design: .monospaced))
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .textSelection(.enabled)
                    }
                    .frame(minHeight: 90, maxHeight: 180)
                }
            }

            if let event = coreHost.lastEventJSON, !event.isEmpty {
                Text("Last Event: \(event)")
                    .font(.footnote)
            }

            if let error = coreHost.lastError, !error.isEmpty {
                Text("Error: \(error)")
                    .foregroundStyle(.red)
                    .font(.footnote)
                    .textSelection(.enabled)
            }

            HStack(spacing: 10) {
                Button("Initialize Core") {
                    Task { await coreHost.initialize() }
                }

                Button("Get Status") {
                    Task { await coreHost.refreshStatus() }
                }
                .disabled(!coreHost.canQuery)

                Button("Shutdown Core") {
                    Task { await coreHost.shutdown() }
                }
                .disabled(!coreHost.canShutdown)
            }
        }
        .padding()
        .frame(minWidth: 640, minHeight: 360)
    }
}

#Preview {
    ContentView()
        .environmentObject(CoreHostService())
}
