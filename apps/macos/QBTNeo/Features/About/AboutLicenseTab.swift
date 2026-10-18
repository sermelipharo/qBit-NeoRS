import SwiftUI

/// "License" tab: GPL for the application, Apache 2.0 for the engine and the
/// DB-IP attribution.
struct AboutLicenseTab: View {
    var body: some View {
        Form {
            Section("qBit NeoRS") {
                Text("""
                This program is free software: you can redistribute it and/or modify it under the terms of the GNU \
                General Public License as published by the Free Software Foundation, either version 3 of the \
                License, or (at your option) any later version.
                """)
                Link("GNU General Public License v3.0", destination: AboutLinks.gpl)
            }
            Section("BitTorrent engine") {
                Text("""
                The BitTorrent engine is a modified fork of rqbit (librqbit), Copyright 2021 Igor Katson, licensed \
                under the Apache License, Version 2.0.
                """)
                Link("Apache License 2.0", destination: AboutLinks.apache)
                Link("rqbit on GitHub", destination: AboutLinks.rqbit)
            }
            Section("Peer countries") {
                Text("""
                The free IP to Country Lite database by DB-IP is used for resolving the countries of peers. The \
                database is licensed under the Creative Commons Attribution 4.0 International License.
                """)
                Link("https://db-ip.com", destination: AboutLinks.dbip)
            }
        }
        .formStyle(.grouped)
    }
}
