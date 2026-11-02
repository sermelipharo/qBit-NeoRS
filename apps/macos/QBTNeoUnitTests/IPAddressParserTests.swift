import Testing

struct IPAddressParserTests {
    @Test func banEntriesMatchTheCore() {
        let e = IPAddressParser.normalizedBanEntry
        #expect(e(" 1.2.3.4 ") == "1.2.3.4")
        #expect(e("10.1.2.3/8") == "10.0.0.0 - 10.255.255.255")
        #expect(e("10.0.0.7/32") == "10.0.0.7")
        #expect(e("0.0.0.0/0") == "0.0.0.0 - 255.255.255.255")
        #expect(e("192.168.0.1-192.168.0.9") == "192.168.0.1 - 192.168.0.9")
        #expect(e("2001:DB8::/126") == "2001:db8:: - 2001:db8::3")
        #expect(e("::ffff:1.2.3.4") == "1.2.3.4")
        for bad in ["", "x", "1.2.3", "1.2.3.9 - 1.2.3.1", "1.2.3.4 - ::1", "1.2.3.4/33", "::/129", "1.2.3.4/x"] {
            #expect(e(bad) == nil, "\(bad)")
        }
    }
}
