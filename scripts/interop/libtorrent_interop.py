#!/usr/bin/env python3
"""Interoperability of the engine with libtorrent (2.x Python bindings).

Local only: every peer listens on 127.0.0.1, libtorrent runs without DHT,
LSD, UPnP, NAT-PMP and uTP, and peers are introduced to each other directly
or through a tracker on 127.0.0.1. Not run in CI. Needs the libtorrent
bindings, e.g. in a throw-away venv:

    python3 -m venv /tmp/lt-venv && /tmp/lt-venv/bin/pip install libtorrent
    /tmp/lt-venv/bin/python scripts/interop/libtorrent_interop.py

Our side is `crates/qbt-e2e/examples/interop_peer.rs` (one engine session),
built by this script with cargo unless --peer-bin is given.

Scenarios, for v1, pure v2 and hybrid torrents made by libtorrent:
  * libtorrent seeds, we download from the .torrent;
  * we seed, libtorrent downloads from the .torrent;
  * libtorrent seeds, we download from its magnet link (btih and/or btmh,
    and a btmh-only link of the hybrid torrent);
  * we seed, libtorrent downloads from the magnet link;
  * hybrid only: a tracker hands out peers under the v2 hash alone, so both
    sides must announce both hashes and connect with the v2 hash;
  * pure v2 only: we download from a .torrent without its piece layers
    (fetched from libtorrent with hash requests), and libtorrent downloads
    from us with such a .torrent (we serve the layers);
  * pure v2 only: a .torrent lacking one file's layer: the other files
    complete from a libtorrent peer that can't send that layer, then the
    layer and the file come from a libtorrent seeder that joins later;
  * hybrid only: we download by a btih magnet over a v1 connection that
    libtorrent upgrades to v2, fetch the piece layers over it, then serve
    them to a second libtorrent whose .torrent lacks them (its v1
    connection to us is upgraded the other way round);
  * hybrid only: both directions of a .torrent download check that the v1
    connection was upgraded to v2 (libtorrent's handshake log).
at 16 KiB and 64 KiB pieces (--piece-kib). Exit status 0 when every
scenario passed.
"""

import argparse
import filecmp
import json
import os
import queue
import subprocess
import sys
import tempfile
import threading
import time
import warnings

import libtorrent as lt

warnings.filterwarnings("ignore", category=DeprecationWarning)

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

FILES = []
PIECE = 16 * 1024
FORMATS = {"v1": "v1_only", "v2": "v2_only", "hybrid": None}


def set_piece_size(piece):
    """Files covering the v2 corner cases at this piece size: a multi-piece
    file with a short last block, a file of exactly one piece, files smaller
    than a piece (one block and several), an empty file, nested folders, a
    file of more than 512 pieces (its piece layer takes several hash
    requests, the last one shorter) and a last file that does not end on a
    piece boundary (tail padding)."""
    global PIECE
    PIECE = piece
    FILES[:] = [
        ("a/big.bin", 18 * piece + 5_088),
        ("a/small.bin", piece + 3_616),
        ("b/c/tiny.txt", 100),
        ("b/empty", 0),
        ("one_piece.bin", piece),
        ("c/large.bin", 519 * piece + 1),
        ("z_last.bin", 4 * piece + 4_465),
    ]


def log(*a):
    print(*a, flush=True)


def make_content(root, name):
    base = os.path.join(root, name)
    for i, (rel, size) in enumerate(FILES):
        p = os.path.join(base, rel)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "wb") as f:
            period = bytes(((j * 31 + i * 7) % 251) for j in range(251))
            f.write((period * (size // 251 + 1))[:size])
    return base


def make_torrent(content, fmt, out_path):
    fs = lt.file_storage()
    lt.add_files(fs, content)
    flag = FORMATS[fmt]
    flags = getattr(lt.create_torrent, flag) if flag else 0
    ct = lt.create_torrent(fs, PIECE, flags=flags)
    ct.set_creator("libtorrent interop")
    lt.set_piece_hashes(ct, os.path.dirname(content))
    data = lt.bencode(ct.generate())
    with open(out_path, "wb") as f:
        f.write(data)
    return lt.torrent_info(out_path)


def lt_session(port, peer_log=False):
    extra = lt.alert.category_t.peer_log_notification if peer_log else 0
    s = lt.session(
        {
            "listen_interfaces": "127.0.0.1:%d" % port,
            "enable_dht": False,
            "enable_lsd": False,
            "enable_upnp": False,
            "enable_natpmp": False,
            "enable_incoming_utp": False,
            "enable_outgoing_utp": False,
            "allow_multiple_connections_per_ip": True,
            "alert_mask": lt.alert.category_t.all_categories
            if VERBOSE
            else (
                lt.alert.category_t.error_notification
                | lt.alert.category_t.peer_notification
                | lt.alert.category_t.status_notification
                | extra
            ),
        }
    )
    return s


def lt_port(s):
    return s.listen_port()


VERBOSE = False


PEER_LOG = []


def drain(ses, interesting, tag):
    for a in ses.pop_alerts():
        if isinstance(a, lt.peer_log_alert):
            PEER_LOG.append((tag, a.message()))
            if len(PEER_LOG) > 200000:
                del PEER_LOG[:100000]
        if VERBOSE or isinstance(a, interesting):
            log("   lt[%s]: %s: %s" % (tag, type(a).__name__, a.message()))


def peer_log_has(tag, *needles):
    return any(t == tag and all(n in m for n in needles) for t, m in PEER_LOG)


def without_piece_layers(tpath, out_path):
    """A copy of the .torrent without its `piece layers`."""
    with open(tpath, "rb") as f:
        d = lt.bdecode(f.read())
    d.pop(b"piece layers", None)
    with open(out_path, "wb") as f:
        f.write(lt.bencode(d))
    return out_path


def without_layer_of(tpath, rel, out_path):
    """A copy of the .torrent without the piece layer of the file `rel`."""
    with open(tpath, "rb") as f:
        d = lt.bdecode(f.read())
    node = d[b"info"][b"file tree"]
    for c in rel.split("/"):
        node = node[c.encode()]
    root = node[b""][b"pieces root"]
    del d[b"piece layers"][root]
    with open(out_path, "wb") as f:
        f.write(lt.bencode(d))
    return out_path


def wait_lt(ses, h, pred, timeout, tag):
    end = time.time() + timeout
    noisy = (lt.peer_error_alert, lt.peer_disconnected_alert, lt.hash_failed_alert, lt.torrent_error_alert)
    while time.time() < end:
        drain(ses, noisy, tag)
        st = h.status()
        if pred(st):
            return True
        time.sleep(0.1)
    st = h.status()
    log("   lt[%s] timed out: state=%s progress=%.3f peers=%d" % (tag, st.state, st.progress, st.num_peers))
    return False


def free_port():
    import socket

    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Tracker:
    """A minimal HTTP tracker on 127.0.0.1 that hands out peers only for the
    hashes in `serve` (the v2 hash of a hybrid torrent): a peer then knows
    the other one only under that hash, as libtorrent's `protocol_v2` peers."""

    def __init__(self, serve):
        import http.server
        import urllib.parse

        self.announced = []
        peers = {}
        outer = self

        class H(http.server.BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_GET(self):
                u = urllib.parse.urlsplit(self.path)
                q = urllib.parse.parse_qs(u.query, encoding="latin-1")
                ih = q.get("info_hash", [""])[0].encode("latin-1")
                port = int(q.get("port", ["0"])[0])
                body = b"d8:intervali5e12:min intervali1e5:peers0:e"
                if u.path == "/announce" and len(ih) == 20 and port:
                    outer.announced.append((ih.hex(), port))
                    known = peers.setdefault(ih, set())
                    out = b""
                    if ih.hex() in serve:
                        for p in sorted(known - {port}):
                            out += bytes([127, 0, 0, 1]) + p.to_bytes(2, "big")
                    if q.get("event", [""])[0] != "stopped":
                        known.add(port)
                    body = b"d8:intervali5e12:min intervali1e5:peers%d:%se" % (len(out), out)
                try:
                    self.send_response(200)
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                except OSError:
                    pass  # a peer that stopped meanwhile

        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
        self.url = "http://127.0.0.1:%d/announce" % self.httpd.server_address[1]
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()

    def close(self):
        self.httpd.shutdown()


class Ours:
    """Our engine (interop_peer) as a child process."""

    def __init__(self, binary, add, out, peers=(), seed_for=0, timeout=90, tracker=None, plaintext=False, stdin_peers=False):
        self.port = free_port()
        cmd = [binary, "--add", add, "--out", out, "--listen", str(self.port), "--timeout", str(timeout)]
        cmd += ["--seed-for", str(seed_for)]
        for p in peers:
            cmd += ["--peer", p]
        if tracker:
            cmd += ["--tracker", tracker]
        if plaintext:
            cmd += ["--plaintext"]
        if stdin_peers:
            cmd += ["--stdin-peers"]
        self.err = tempfile.TemporaryFile(mode="w+")
        self.p = subprocess.Popen(
            cmd, stdout=subprocess.PIPE, stderr=self.err, text=True, stdin=subprocess.PIPE if stdin_peers else None
        )
        self.lines = []
        self.q = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.p.stdout:
            self.q.put(line.rstrip("\n"))
        self.q.put(None)

    def wait_line(self, prefix, timeout):
        end = time.time() + timeout
        while time.time() < end:
            try:
                line = self.q.get(timeout=max(0.01, end - time.time()))
            except queue.Empty:
                break
            if line is None:
                break
            self.lines.append(line)
            if line.startswith(prefix):
                return line
        return None

    def add_peer(self, addr):
        """Needs stdin_peers."""
        self.p.stdin.write("PEER %s\n" % addr)
        self.p.stdin.flush()

    def stop(self):
        if self.p.stdin:
            try:
                self.p.stdin.close()
            except OSError:
                pass
        if self.p.poll() is None:
            self.p.terminate()
            try:
                self.p.wait(5)
            except subprocess.TimeoutExpired:
                self.p.kill()
        self.err.seek(0)
        return self.err.read()


def same_tree(a, b, quiet=False):
    for rel, _ in FILES:
        pa, pb = os.path.join(a, rel), os.path.join(b, rel)
        if not os.path.exists(pb) or not filecmp.cmp(pa, pb, shallow=False):
            if not quiet:
                log("   differs: %s" % rel)
            return False
    return True


def lt_flushed_tree(ses, h, a, b):
    """Compares after libtorrent wrote its disk cache out (it reports a
    piece as done before the data reaches the file)."""
    h.save_resume_data(lt.torrent_handle.flush_disk_cache)
    end = time.time() + 15
    while time.time() < end:
        if any(isinstance(x, lt.save_resume_data_alert) for x in ses.pop_alerts()) and same_tree(a, b, quiet=True):
            return True
        if same_tree(a, b, quiet=True):
            return True
        time.sleep(0.2)
    return same_tree(a, b)


def scenario_lt_seeds(binary, work, content, ti, source, label, tracker=None, check=None):
    """libtorrent seeds; we download from `source` (.torrent path or magnet),
    finding libtorrent directly or through `tracker`. `check(ours, tag)`:
    more conditions once the download is done (libtorrent's peer log is
    recorded under the tag "seed-<label>")."""
    del PEER_LOG[:]
    tag = "seed-" + label
    ses = lt_session(0, peer_log=check is not None)
    atp = lt.add_torrent_params()
    atp.ti = ti
    atp.save_path = os.path.dirname(content)
    if tracker:
        atp.trackers = [tracker.url]
    h = ses.add_torrent(atp)
    if not wait_lt(ses, h, lambda st: st.is_seeding, 30, "seed"):
        return False
    out = os.path.join(work, "ours-" + label)
    os.makedirs(out, exist_ok=True)
    multi = ti.num_files() > 1
    target = os.path.join(out, ti.name()) if multi else out
    peers = [] if tracker else ["127.0.0.1:%d" % lt_port(ses)]
    # libtorrent upgrades our v1 connection only without MSE (with it, the
    # hash the encryption is keyed with decides).
    ours = Ours(
        binary, source, target, peers=peers, tracker=tracker and tracker.url, seed_for=20 if check else 0, plaintext=check is not None
    )
    end = time.time() + 90
    ok = False
    while time.time() < end and not ok:
        ok = ours.wait_line("DONE", 0.5) is not None
        drain(ses, (lt.peer_error_alert, lt.peer_disconnected_alert, lt.invalid_request_alert), tag)
        if ours.p.poll() is not None and not ok:
            ok = ours.wait_line("DONE", 0.5) is not None
            break
    if ok:
        ok = same_tree(content, target)
    if ok and check:
        drain(ses, (), tag)
        ok = check(ours, tag, ses)
    err = ours.stop()
    if not ok:
        log("   our output: %s" % " | ".join(ours.lines[-5:]))
        log("   our stderr: %s" % err.strip()[-3000:])
    del ses
    return ok


def scenario_we_seed(binary, work, content, tpath, ti, magnet, label, tracker=None, lt_tpath=None, check=None, ours=None):
    """We seed from the .torrent; libtorrent downloads from the .torrent
    (`lt_tpath` if given) or `magnet`, finding us directly or through
    `tracker`. `ours`: an already running seeder of ours. `check(tag)`:
    more conditions once done (libtorrent's peer log is recorded under the
    tag "leech-<label>")."""
    multi = ti.num_files() > 1
    if ours is None:
        ours = Ours(binary, tpath, content if multi else os.path.dirname(content), seed_for=120, tracker=tracker and tracker.url)
        if ours.wait_line("DONE", 60) is None:
            log("   our seed failed: %s" % ours.stop()[-2000:])
            return False
    del PEER_LOG[:]
    tag = "leech-" + label
    ses = lt_session(0, peer_log=check is not None)
    out = os.path.join(work, "lt-" + label)
    os.makedirs(out, exist_ok=True)
    if magnet:
        atp = lt.parse_magnet_uri(magnet)
    else:
        atp = lt.add_torrent_params()
        atp.ti = lt.torrent_info(lt_tpath or tpath)
    atp.save_path = out
    if tracker:
        atp.trackers = [tracker.url]
    h = ses.add_torrent(atp)
    if not tracker:
        h.connect_peer(("127.0.0.1", ours.port))
    ok = wait_lt(ses, h, lambda st: st.is_seeding or st.is_finished, 90, tag)
    if ok:
        ok = lt_flushed_tree(ses, h, content, os.path.join(out, ti.name()))
    if ok and check:
        end = time.time() + 20
        while True:
            drain(ses, (), tag)
            ok = check(tag)
            if ok or time.time() > end:
                break
            time.sleep(0.2)
    err = ours.stop()
    if not ok:
        log("   our stderr: %s" % err.strip()[-3000:])
    del ses
    return ok


def scenario_layer_later(binary, work, content, ti, partial, missing_rel, label):
    """A pure v2 .torrent lacking the piece layer of `missing_rel`: we start
    at once with a libtorrent peer that has the same .torrent and the data,
    but one piece of that file damaged (a complete file would give
    libtorrent the layer: it hashes the file up to its root), so it can't
    send that layer. Every other file must complete while the layer is
    missing (`PARTIAL`); then a libtorrent seeder with the complete .torrent
    joins, we fetch the layer from it (its peer log shows our hash requests,
    none rejected) and the remaining file completes."""
    import shutil

    del PEER_LOG[:]
    damaged = os.path.join(work, "lt-partial-" + label, os.path.basename(content))
    shutil.copytree(content, damaged)
    with open(os.path.join(damaged, missing_rel), "r+b") as f:
        f.seek(3 * PIECE + 100)
        b = f.read(1)
        f.seek(3 * PIECE + 100)
        f.write(bytes([b[0] ^ 0xFF]))
    ses1 = lt_session(0)
    atp = lt.add_torrent_params()
    atp.ti = lt.torrent_info(partial)
    atp.save_path = os.path.dirname(damaged)
    h1 = ses1.add_torrent(atp)
    checked = (lt.torrent_status.downloading, lt.torrent_status.seeding, lt.torrent_status.finished)
    # Its check fails every piece of the damaged file (the file doesn't hash
    # up to its root): expected, not reported.
    end = time.time() + 30
    while h1.status().state not in checked:
        ses1.pop_alerts()
        if time.time() > end:
            log("   lt[partial] never finished checking")
            return False
        time.sleep(0.1)
    ses1.pop_alerts()
    out = os.path.join(work, "ours-" + label)
    os.makedirs(out, exist_ok=True)
    target = os.path.join(out, ti.name())
    ours = Ours(binary, partial, target, peers=["127.0.0.1:%d" % lt_port(ses1)], timeout=150, stdin_peers=True)
    sessions = [ses1]

    def run():
        line = None
        end = time.time() + 60
        while time.time() < end and line is None and ours.p.poll() is None:
            line = ours.wait_line("PARTIAL", 0.5)
            drain(ses1, (lt.peer_error_alert,), "partial-" + label)
        if line is None:
            log("   the other files never completed while the layer was missing")
            return False
        for rel, _ in FILES:
            if rel != missing_rel and not filecmp.cmp(os.path.join(content, rel), os.path.join(target, rel), shallow=False):
                log("   differs before the layer arrived: %s" % rel)
                return False
        if ours.wait_line("DONE", 0.1) is not None:
            log("   done without the layer?")
            return False
        tag = "full-" + label
        ses2 = lt_session(0, peer_log=True)
        sessions.append(ses2)
        atp = lt.add_torrent_params()
        atp.ti = ti
        atp.save_path = os.path.dirname(content)
        h2 = ses2.add_torrent(atp)
        if not wait_lt(ses2, h2, lambda st: st.is_seeding, 30, tag):
            return False
        ours.add_peer("127.0.0.1:%d" % lt_port(ses2))
        ok = False
        end = time.time() + 90
        while time.time() < end and not ok and ours.p.poll() is None:
            ok = ours.wait_line("DONE", 0.5) is not None
            drain(ses1, (), "partial-" + label)
            drain(ses2, (lt.peer_error_alert, lt.invalid_request_alert), tag)
        ok = ok or ours.wait_line("DONE", 0.5) is not None
        ok = ok and same_tree(content, target)
        drain(ses2, (), tag)
        if ok and (not peer_log_has(tag, "HASH_REQUEST") or peer_log_has(tag, "HASH_REJECT")):
            log("   libtorrent did not serve our hash requests")
            ok = False
        return ok

    ok = run()
    err = ours.stop()
    if not ok:
        log("   our output: %s" % " | ".join(ours.lines[-5:]))
        log("   our stderr: %s" % err.strip()[-3000:])
    del sessions[:]
    return ok


def build_peer():
    out = subprocess.run(
        ["cargo", "build", "-p", "qbt-e2e", "--example", "interop_peer", "--message-format=json"],
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    ).stdout
    for line in out.splitlines():
        try:
            m = json.loads(line)
        except ValueError:
            continue
        if m.get("reason") == "compiler-artifact" and m.get("executable") and m["target"]["name"] == "interop_peer":
            return m["executable"]
    raise SystemExit("interop_peer was not built")


def cases_for(fmt, binary, work, content, tpath, ti):
    ih = ti.info_hashes()
    v2 = str(ih.v2) if ih.has_v2() else ""
    magnet = lt.make_magnet_uri(ti)
    xt = "&".join(p for p in magnet.split("?", 1)[1].split("&") if p.startswith("xt="))
    cases = [
        ("lt seeds, we download (.torrent)", lambda: scenario_lt_seeds(binary, work, content, ti, tpath, fmt + "-file")),
        ("we seed, lt downloads (.torrent)", lambda: scenario_we_seed(binary, work, content, tpath, ti, None, fmt + "-file")),
        ("lt seeds, we download (magnet %s)" % xt, lambda: scenario_lt_seeds(binary, work, content, ti, magnet, fmt + "-magnet")),
        ("we seed, lt downloads (magnet %s)" % xt, lambda: scenario_we_seed(binary, work, content, tpath, ti, magnet, fmt + "-magnet")),
    ]
    if fmt == "v2":
        no_layers = without_piece_layers(tpath, os.path.join(work, "v2-no-layers.torrent"))
        late = "c/large.bin"
        one_missing = without_layer_of(tpath, late, os.path.join(work, "v2-one-layer-missing.torrent"))
        cases += [
            (
                "lt seeds, we download (.torrent without piece layers)",
                lambda: scenario_lt_seeds(binary, work, content, ti, no_layers, fmt + "-nolayers"),
            ),
            (
                "we seed, lt downloads (.torrent without piece layers)",
                lambda: scenario_we_seed(binary, work, content, tpath, ti, None, fmt + "-lt-nolayers", lt_tpath=no_layers),
            ),
            (
                "one layer missing: the other files complete first, the layer and its file come from a later lt",
                lambda: scenario_layer_later(binary, work, content, ti, one_missing, late, fmt + "-late-layer"),
            ),
        ]
    if fmt == "hybrid":
        btmh = "magnet:?xt=urn:btmh:1220" + v2
        alt = v2[:40]
        btih = "magnet:?xt=urn:btih:%s" % ih.v1
        no_layers = without_piece_layers(tpath, os.path.join(work, "hybrid-no-layers.torrent"))

        def upgraded_by_lt(ours, tag, ses):
            # libtorrent answered our v1 handshake (v2 bit set) with the v2 hash.
            if not peer_log_has(tag, "HANDSHAKE", "ih: " + alt):
                log("   libtorrent never answered with the v2 hash")
                for t, m in [x for x in PEER_LOG if x[0] == tag][:25]:
                    log("   lt[%s]: %s" % (t, m))
                return False
            return True

        def fetched_layers(ours, tag, ses):
            if not upgraded_by_lt(ours, tag, ses):
                return False
            if ours.wait_line("LAYERS", 15) is None:
                log("   we never fetched the piece layers")
                return False
            if not peer_log_has(tag, "HASH_REQUEST") or peer_log_has(tag, "HASH_REJECT"):
                log("   libtorrent did not serve our hash requests")
                return False
            return True

        def layers_from_magnet_then_serve():
            """We download by btih magnet (fetching the layers from lt over
            the upgraded connection), lt goes away, then a second lt with
            a .torrent lacking the layers downloads from us: its v1
            connection is upgraded and we answer its hash requests."""
            out = os.path.join(work, "ours-hybrid-fetch")
            os.makedirs(out, exist_ok=True)
            target = os.path.join(out, ti.name())
            del PEER_LOG[:]
            tag1 = "seed-hybrid-fetch"
            ses = lt_session(0, peer_log=True)
            atp = lt.add_torrent_params()
            atp.ti = ti
            atp.save_path = os.path.dirname(content)
            h = ses.add_torrent(atp)
            if not wait_lt(ses, h, lambda st: st.is_seeding, 30, tag1):
                return False
            ours = Ours(binary, btih, target, peers=["127.0.0.1:%d" % lt_port(ses)], seed_for=120, plaintext=True)
            ok = ours.wait_line("DONE", 90) is not None and same_tree(content, target)
            end = time.time() + 15
            while ok and time.time() < end and not peer_log_has(tag1, "HASH_REQUEST"):
                drain(ses, (), tag1)
                time.sleep(0.1)
            drain(ses, (), tag1)
            ok = ok and fetched_layers(ours, tag1, ses)
            del ses
            if not ok:
                log("   our stderr: %s" % ours.stop().strip()[-3000:])
                return False

            def served(tag):
                return (
                    peer_log_has(tag, "HASH_REQUEST")
                    and peer_log_has(tag, "HASHES")
                    and not peer_log_has(tag, "HASH_REJECT")
                )

            return scenario_we_seed(
                binary, work, target, tpath, ti, None, "hybrid-serve", lt_tpath=no_layers, check=served, ours=ours
            )

        def via_tracker(run):
            t = Tracker({alt})
            try:
                ok = run(t)
                hashes = {h for h, _ in t.announced}
                if ok and not {str(ih.v1), alt} <= hashes:
                    log("   announced only %s" % hashes)
                    ok = False
                return ok
            finally:
                t.close()

        cases += [
            ("lt seeds, we download (btmh-only magnet)", lambda: scenario_lt_seeds(binary, work, content, ti, btmh, fmt + "-btmh")),
            ("we seed, lt downloads (btmh-only magnet)", lambda: scenario_we_seed(binary, work, content, tpath, ti, btmh, fmt + "-btmh")),
            (
                "lt seeds, we download, peers only under the v2 hash (tracker)",
                lambda: via_tracker(lambda t: scenario_lt_seeds(binary, work, content, ti, tpath, fmt + "-trk", tracker=t)),
            ),
            (
                "we seed, lt downloads, peers only under the v2 hash (tracker)",
                lambda: via_tracker(lambda t: scenario_we_seed(binary, work, content, tpath, ti, None, fmt + "-trk", tracker=t)),
            ),
            (
                "lt seeds, we download (.torrent): lt upgrades our v1 connection to v2",
                lambda: scenario_lt_seeds(binary, work, content, ti, tpath, fmt + "-upgrade", check=upgraded_by_lt),
            ),
            (
                "lt seeds, we download (btih magnet), fetch the piece layers over the upgraded connection, then serve them to lt",
                layers_from_magnet_then_serve,
            ),
            (
                "we seed, lt downloads by magnet, peers only under the v2 hash (tracker)",
                lambda: via_tracker(
                    lambda t: scenario_we_seed(binary, work, content, tpath, ti, magnet, fmt + "-trk-magnet", tracker=t)
                ),
            ),
        ]
    return cases


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--peer-bin")
    ap.add_argument("--only", help="comma-separated formats (v1,v2,hybrid)")
    ap.add_argument("--case", help="only scenarios whose name contains this text")
    ap.add_argument("--verbose", action="store_true", help="every libtorrent alert (set RUST_LOG for our side)")
    ap.add_argument(
        "--piece-kib",
        default="16,64",
        help="comma-separated piece sizes in KiB (16: the piece layer is the block layer; 64: four blocks per piece)",
    )
    args = ap.parse_args()
    global VERBOSE
    VERBOSE = args.verbose
    binary = args.peer_bin or build_peer()
    log("libtorrent %s, peer %s" % (lt.__version__, binary))
    results = []
    for kib in [int(k) for k in args.piece_kib.split(",")]:
        set_piece_size(kib * 1024)
        with tempfile.TemporaryDirectory() as work:
            for fmt in args.only.split(",") if args.only else FORMATS:
                src = os.path.join(work, "src-" + fmt)
                content = make_content(src, "Interop " + fmt)
                tpath = os.path.join(work, fmt + ".torrent")
                ti = make_torrent(content, fmt, tpath)
                ih = ti.info_hashes()
                tag = "%s/%dK" % (fmt, kib)
                log("== %s: v1=%s v2=%s" % (tag, ih.v1 if ih.has_v1() else "-", ih.v2 if ih.has_v2() else "-"))
                for name, run in cases_for(fmt, binary, work, content, tpath, ti):
                    if args.case and args.case not in name:
                        continue
                    log("-- %s: %s" % (tag, name))
                    t0 = time.time()
                    ok = run()
                    log("   %s (%.1fs)" % ("PASS" if ok else "FAIL", time.time() - t0))
                    results.append((tag, name, ok))
    log("\nSummary:")
    for tag, name, ok in results:
        log("  %-10s %-4s %s" % (tag, "ok" if ok else "FAIL", name))
    return 0 if all(r[2] for r in results) else 1


if __name__ == "__main__":
    sys.exit(main())
