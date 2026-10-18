//! Peer client identification from the 20-byte BitTorrent peer id.
//!
//! Recognizes the Azureus style (`-qB5240-...`), the Shadow style
//! (`T03I-----...`), the Mainline style (`M4-3-6--...`) and a handful of
//! well-known custom prefixes. The extended handshake `v` string, when a peer
//! sends one, is more accurate and should be preferred by callers.

/// Azureus-style two-letter client codes.
fn azureus_client_name(code: [u8; 2]) -> Option<&'static str> {
    Some(match &code {
        b"7T" => "aTorrent",
        b"AB" => "AnyEvent::BitTorrent",
        b"AG" | b"A~" => "Ares",
        b"AR" => "Arctic Torrent",
        b"AT" => "Artemis",
        b"AV" => "Avicora",
        b"AX" => "BitPump",
        b"AZ" => "Azureus",
        b"BB" => "BitBuddy",
        b"BC" => "BitComet",
        b"BE" => "baretorrent",
        b"BF" => "Bitflu",
        b"BG" => "BTG",
        b"BI" => "BiglyBT",
        b"BL" => "BitCometLite",
        b"BN" => "Baidu Netdisk",
        b"BP" => "BitTorrent Pro",
        b"BR" => "BitRocket",
        b"BS" => "BTSlave",
        b"BT" => "BitTorrent",
        b"BW" => "BitWombat",
        b"BX" => "BittorrentX",
        b"CD" => "Enhanced CTorrent",
        b"CT" => "CTorrent",
        b"DE" => "Deluge",
        b"DP" => "Propagate Data Client",
        b"EB" => "EBit",
        b"ES" => "electric sheep",
        b"FC" => "FileCroc",
        b"FD" => "Free Download Manager",
        b"FG" => "FlashGet",
        b"FT" => "FoxTorrent",
        b"FW" => "FrostWire",
        b"FX" => "Freebox BitTorrent",
        b"GR" => "GetRight",
        b"GS" => "GSTorrent",
        b"HK" => "Hekate",
        b"HL" => "Halite",
        b"HM" => "hMule",
        b"HN" => "Hydranode",
        b"IL" => "iLivid",
        b"JS" => "Justseed.it client",
        b"JT" => "JavaTorrent",
        b"KG" => "KGet",
        b"KT" => "KTorrent",
        b"LC" => "LeechCraft",
        b"LH" => "LH-ABC",
        b"LP" => "Lphant",
        b"LT" => "libtorrent",
        b"lt" => "libTorrent",
        b"LW" => "LimeWire",
        b"Lr" => "LibreTorrent",
        b"MG" => "MediaGet",
        b"MK" => "Meerkat",
        b"MO" => "MonoTorrent",
        b"MP" => "MooPolice",
        b"MR" => "Miro",
        b"MT" => "MoonlightTorrent",
        b"NB" => "Net::BitTorrent",
        b"NX" => "Net Transport",
        b"OS" => "OneSwarm",
        b"OT" => "OmegaTorrent",
        b"PB" => "Protocol::BitTorrent",
        b"PD" => "Pando",
        b"PI" => "PicoTorrent",
        b"PT" => "PHPTracker",
        b"QD" => "QQDownload",
        b"QT" => "Qt 4 Torrent example",
        b"RT" => "Retriever",
        b"RZ" => "RezTorrent",
        b"S~" => "Shareaza alpha/beta",
        b"SB" => "Swiftbit",
        b"SD" => "Thunder",
        b"SM" => "SoMud",
        b"SP" => "BitSpirit",
        b"SS" => "SwarmScope",
        b"ST" => "SymTorrent",
        b"SZ" => "Shareaza",
        b"st" => "sharktorrent",
        b"TB" => "Torch",
        b"TE" => "terasaur Seed Bank",
        b"TL" => "Tribler",
        b"TN" => "TorrentDotNET",
        b"TR" => "Transmission",
        b"TS" => "Torrentstorm",
        b"TT" => "TuoTu",
        b"UE" => "\u{b5}Torrent Embedded",
        b"UL" => "uLeecher!",
        b"UM" => "\u{b5}Torrent Mac",
        b"UT" => "\u{b5}Torrent",
        b"UW" => "\u{b5}Torrent Web",
        b"VG" => "Vagaa",
        b"WD" => "WebTorrent Desktop",
        b"WT" => "BitLet",
        b"WW" => "WebTorrent",
        b"WY" => "FireTorrent",
        b"XF" => "Xfplay",
        b"XL" => "Xunlei",
        b"XS" => "XSwifter",
        b"XT" => "XanTorrent",
        b"XX" => "Xtorrent",
        b"ZT" => "ZipTorrent",
        b"qB" => "qBittorrent",
        b"rQ" => "rqbit",
        b"tT" => "tTorrent",
        _ => return None,
    })
}

/// Shadow-style single-letter client codes.
fn shadow_client_name(code: u8) -> Option<&'static str> {
    Some(match code {
        b'A' => "ABC",
        b'O' => "Osprey Permaseed",
        b'Q' => "BTQueue",
        b'R' => "Tribler",
        b'S' => "Shad0w",
        b'T' => "BitTornado",
        b'U' => "UPnP NAT Bit Torrent",
        _ => return None,
    })
}

/// Prefixes (at a given offset) of clients that use their own peer id format.
const CUSTOM_IDS: &[(usize, &[u8], &str)] = &[
    (0, b"Deadman Walking-", "Deadman"),
    (5, b"Azureus", "Azureus 2.0.3.2"),
    (0, b"DansClient", "XanTorrent"),
    (4, b"btfans", "SimpleBT"),
    (0, b"PRC.P---", "Bittorrent Plus! II"),
    (0, b"P87.P---", "Bittorrent Plus!"),
    (0, b"S587Plus", "Bittorrent Plus!"),
    (0, b"martini", "Martini Man"),
    (0, b"Plus---", "Bittorrent Plus"),
    (0, b"turbobt", "TurboBT"),
    (0, b"a00---0", "Swarmy"),
    (0, b"a02---0", "Swarmy"),
    (0, b"T00---0", "Teeweety"),
    (0, b"BTDWV-", "Deadman Walking"),
    (2, b"BS", "BitSpirit"),
    (0, b"Pando-", "Pando"),
    (0, b"LIME", "LimeWire"),
    (0, b"btuga", "BTugaXP"),
    (0, b"oernu", "BTugaXP"),
    (0, b"Mbrst", "Burst!"),
    (0, b"PEERAPP", "PeerApp"),
    (0, b"Plus", "Plus!"),
    (0, b"-Qt-", "Qt"),
    (0, b"exbc", "BitComet"),
    (0, b"FUTB", "BitComet"),
    (0, b"-G3", "G3 Torrent"),
    (0, b"-WS", "HTTP Seed"),
    (0, b"HTTPBT", "HTTP Seed"),
    (0, b"-BOW", "Bits on Wheels"),
    (0, b"346-", "TorrentTopia"),
    (0, b"eX", "eXeem"),
];

/// Decodes one version character: `0-9`, then `A-Z`/`a-z` as 10-35.
fn version_digit(c: u8) -> Option<u32> {
    match c {
        b'0'..=b'9' => Some(u32::from(c - b'0')),
        b'A'..=b'Z' => Some(u32::from(c - b'A') + 10),
        b'a'..=b'z' => Some(u32::from(c - b'a') + 10),
        _ => None,
    }
}

fn is_printable(c: u8) -> bool {
    (32..=126).contains(&c)
}

fn decode_azureus(id: &[u8; 20]) -> Option<String> {
    if id[0] != b'-' || id[7] != b'-' || !is_printable(id[1]) || !is_printable(id[2]) {
        return None;
    }
    let code = [id[1], id[2]];
    let v: [u8; 4] = [id[3], id[4], id[5], id[6]];
    if !v.iter().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let d = |c: u8| version_digit(c).unwrap_or(0);
    let name: String = match azureus_client_name(code) {
        Some(n) => n.to_owned(),
        None => {
            if !code.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'~') {
                return None;
            }
            String::from_utf8_lossy(&code)
                .trim_end_matches('-')
                .to_owned()
        }
    };

    let version = match &code {
        // Transmission: 1.x-3.x are "major.minor" with two minor digits,
        // 4.x and later "major.minor.patch". The last character marks builds.
        b"TR" => {
            let suffix = match v[3] {
                b'Z' | b'X' => "+",
                b'B' => " Beta",
                _ => "",
            };
            if &v[..3] == b"000" {
                format!("0.{}{suffix}", d(v[3]))
            } else if d(v[0]) <= 3 {
                format!("{}.{}{}{suffix}", d(v[0]), d(v[1]), d(v[2]))
            } else {
                format!("{}.{}.{}{suffix}", d(v[0]), d(v[1]), d(v[2]))
            }
        }
        // BitComet / BitCometLite: "-BC0201-" is 2.01.
        b"BC" | b"BL" => format!("{}.{}{}", d(v[1]), d(v[2]), d(v[3])),
        // The uTorrent family and mainline BitTorrent 6+ use the last
        // character as a build type letter (B = beta, S = stable...).
        b"UT" | b"UM" | b"UE" | b"UL" | b"UW" | b"BT" => {
            let mut s = format!("{}.{}.{}", d(v[0]), d(v[1]), d(v[2]));
            if v[3] == b'B' {
                s.push_str(" Beta");
            } else if v[3].is_ascii_digit() && v[3] != b'0' {
                s.push_str(&format!(".{}", d(v[3])));
            }
            s
        }
        _ => {
            let mut s = format!("{}.{}.{}", d(v[0]), d(v[1]), d(v[2]));
            // A non-zero numeric 4th component is a version part; letters
            // there are build tags and are left out.
            if v[3].is_ascii_digit() && v[3] != b'0' {
                s.push_str(&format!(".{}", d(v[3])));
            }
            s
        }
    };
    Some(format!("{name} {version}"))
}

fn decode_shadow(id: &[u8; 20]) -> Option<String> {
    let name = shadow_client_name(id[0])?;
    // "T03I-----": up to 5 version characters terminated by '-'.
    let version_chars: Vec<u8> = id[1..6]
        .iter()
        .copied()
        .take_while(|c| *c != b'-')
        .collect();
    if version_chars.is_empty()
        || id[1..9]
            .iter()
            .skip(version_chars.len())
            .any(|c| *c != b'-')
    {
        return None;
    }
    let parts = version_chars
        .iter()
        .map(|c| version_digit(*c).map(|d| d.to_string()))
        .collect::<Option<Vec<_>>>()?;
    Some(format!("{name} {}", parts.join(".")))
}

fn decode_mainline(id: &[u8; 20]) -> Option<String> {
    // "M4-3-6--" or "M4-20-8-".
    if id[0] != b'M' {
        return None;
    }
    let s = std::str::from_utf8(&id[1..8]).ok()?;
    let mut parts = s.split('-').filter(|p| !p.is_empty());
    let a = parts.next()?;
    let b = parts.next()?;
    let c = parts.next()?;
    if parts.next().is_some()
        || ![a, b, c]
            .iter()
            .all(|p| p.bytes().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    Some(format!("Mainline {a}.{b}.{c}"))
}

fn decode_custom(id: &[u8; 20]) -> Option<String> {
    for (offset, prefix, name) in CUSTOM_IDS {
        if id.get(*offset..*offset + prefix.len()) == Some(*prefix) {
            return Some(match *prefix {
                b"exbc" | b"FUTB" => format!("{name} {}.{:02}", id[4], id[5]),
                _ => (*name).to_owned(),
            });
        }
    }
    if &id[..3] == b"XBT" && id[3..6].iter().all(u8::is_ascii_digit) {
        return Some(format!(
            "XBT Client {}.{}.{}",
            char::from(id[3]),
            char::from(id[4]),
            char::from(id[5])
        ));
    }
    if &id[..2] == b"OP" && id[2..6].iter().all(u8::is_ascii_digit) {
        return Some(format!(
            "Opera {}",
            std::str::from_utf8(&id[2..6]).unwrap_or_default()
        ));
    }
    if &id[..3] == b"-ML" {
        let rest = &id[3..];
        let end = rest.iter().position(|c| *c == b'-').unwrap_or(rest.len());
        let version = std::str::from_utf8(&rest[..end]).ok()?;
        return Some(format!("MLDonkey {version}"));
    }
    None
}

/// Returns a human-readable client name for a 20-byte peer id, e.g.
/// `"qBittorrent 5.2.4"`, `"Transmission 4.0.6"` or `"µTorrent 3.5.5"`.
///
/// An all-zero id (not known yet) gives `"Unknown"`; an unrecognized one gives
/// `"Unknown [<first 8 bytes, non-printable as '.'>]"`.
pub fn client_name_from_peer_id(id: &[u8; 20]) -> String {
    if id.iter().all(|b| *b == 0) {
        return "Unknown".to_owned();
    }
    if let Some(name) = decode_custom(id)
        .or_else(|| decode_azureus(id))
        .or_else(|| decode_mainline(id))
        .or_else(|| decode_shadow(id))
    {
        return name;
    }
    let printable: String = id[..8]
        .iter()
        .map(|c| {
            if is_printable(*c) {
                char::from(*c)
            } else {
                '.'
            }
        })
        .collect();
    format!("Unknown [{printable}]")
}

#[cfg(test)]
mod tests {
    use super::client_name_from_peer_id;

    fn id(prefix: &[u8]) -> [u8; 20] {
        let mut out = [0x8fu8; 20];
        out[..prefix.len()].copy_from_slice(prefix);
        out
    }

    #[test]
    fn azureus_style() {
        assert_eq!(
            client_name_from_peer_id(&id(b"-qB5240-")),
            "qBittorrent 5.2.4"
        );
        assert_eq!(
            client_name_from_peer_id(&id(b"-qB51A0-")),
            "qBittorrent 5.1.10"
        );
        assert_eq!(
            client_name_from_peer_id(&id(b"-TR4060-")),
            "Transmission 4.0.6"
        );
        assert_eq!(
            client_name_from_peer_id(&id(b"-TR2940-")),
            "Transmission 2.94"
        );
        assert_eq!(
            client_name_from_peer_id(&id(b"-TR300Z-")),
            "Transmission 3.00+"
        );
        assert_eq!(client_name_from_peer_id(&id(b"-UT355S-")), "µTorrent 3.5.5");
        assert_eq!(client_name_from_peer_id(&id(b"-UT2210-")), "µTorrent 2.2.1");
        assert_eq!(client_name_from_peer_id(&id(b"-DE13D0-")), "Deluge 1.3.13");
        assert_eq!(
            client_name_from_peer_id(&id(b"-LT2090-")),
            "libtorrent 2.0.9"
        );
        assert_eq!(client_name_from_peer_id(&id(b"-AZ5770-")), "Azureus 5.7.7");
        assert_eq!(client_name_from_peer_id(&id(b"-BC0201-")), "BitComet 2.01");
        assert_eq!(client_name_from_peer_id(&id(b"-rQ9010-")), "rqbit 9.0.1");
        assert_eq!(client_name_from_peer_id(&id(b"-KT4130-")), "KTorrent 4.1.3");
        // Unknown two-letter codes keep the code.
        assert_eq!(client_name_from_peer_id(&id(b"-ZZ1230-")), "ZZ 1.2.3");
    }

    #[test]
    fn shadow_mainline_and_custom() {
        assert_eq!(
            client_name_from_peer_id(&id(b"T03I-----")),
            "BitTornado 0.3.18"
        );
        assert_eq!(client_name_from_peer_id(&id(b"M4-3-6--")), "Mainline 4.3.6");
        assert_eq!(
            client_name_from_peer_id(&id(b"M7-10-2-")),
            "Mainline 7.10.2"
        );
        assert_eq!(
            client_name_from_peer_id(&id(b"exbc\x00\x2aLORD")),
            "BitComet 0.42"
        );
        assert_eq!(
            client_name_from_peer_id(&id(b"XBT054d-")),
            "XBT Client 0.5.4"
        );
        assert_eq!(
            client_name_from_peer_id(&id(b"-ML2.7.2-")),
            "MLDonkey 2.7.2"
        );
        assert_eq!(client_name_from_peer_id(&id(b"OP1011")), "Opera 1011");
    }

    #[test]
    fn unknown() {
        assert_eq!(client_name_from_peer_id(&[0u8; 20]), "Unknown");
        assert_eq!(
            client_name_from_peer_id(&id(b"\x01\x02abcdef")),
            "Unknown [..abcdef]"
        );
    }
}
