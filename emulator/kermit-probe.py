#!/usr/bin/env python3
"""Minimal Kermit client for smoke-testing the emulated calculator over TCP.

Usage: kermit-probe.py [host] [port] [command...]
  commands: init            I-packet parameter exchange only (default)
            dir             REMOTE DIRECTORY (generic G/D), prints the listing
            host TEXT       REMOTE HOST (C packet), e.g. host "1 2 +"
            get NAME        GET a variable (R packet), prints the received bytes
            finish          FINISH (generic G/F), ends server mode
Only type-1 block checks and no 8th-bit prefixing; enough for a smoke test.
"""
import socket, sys, time

SOH, CR = 0x01, 0x0D
tochar = lambda n: n + 32
unchar = lambda c: c - 32

def check1(body: bytes) -> int:
    s = sum(body)
    return tochar((s + ((s & 192) >> 6)) & 63)

def packet(seq: int, ptype: str, data: bytes = b"") -> bytes:
    body = bytes([tochar(len(data) + 3), tochar(seq % 64), ord(ptype)]) + data
    return bytes([SOH]) + body + bytes([check1(body), CR])

class Link:
    def __init__(self, host, port, timeout=10):
        self.s = socket.create_connection((host, port), timeout=timeout)
        self.buf = b""

    def drain(self, secs=0.5):
        """Discard stale input, e.g. an idle-timeout NAK the server sent while
        no client was connected (socat/pty buffer it for the next client)."""
        self.s.settimeout(secs)
        try:
            while True:
                chunk = self.s.recv(4096)
                if not chunk:
                    break
                print("(discarded stale)", chunk)
        except socket.timeout:
            pass
        self.s.settimeout(10)
        self.buf = b""

    def send(self, pkt: bytes):
        print(">>", pkt)
        self.s.sendall(pkt)

    def recv(self):
        """Return (seq, type, data) of the next well-formed packet."""
        while True:
            i = self.buf.find(bytes([SOH]))
            if i >= 0 and len(self.buf) > i + 1:
                n = unchar(self.buf[i + 1])
                end = i + 2 + n
                if len(self.buf) >= end:
                    raw, self.buf = self.buf[i:end], self.buf[end:]
                    body, chk = raw[1:-1], raw[-1]
                    print("<<", raw)
                    if check1(body) != chk:
                        raise IOError(f"bad checksum in {raw!r}")
                    return unchar(body[1]), chr(body[2]), body[3:]
            chunk = self.s.recv(4096)
            if not chunk:
                raise IOError("connection closed")
            self.buf += chunk

def decode(data: bytes, qctl=ord("#")) -> bytes:
    out, i = bytearray(), 0
    while i < len(data):
        c = data[i]
        if c == qctl and i + 1 < len(data):
            i += 1
            c = data[i]
            if (c & 0x7F) not in (qctl, ord("&")):
                c ^= 0x40
        out.append(c)
        i += 1
    return bytes(out)

def encode(text: bytes) -> bytes:
    return b"".join(bytes([c]) for c in text)  # printable ASCII only

INIT = b"~* @-#N1"  # MAXL=94 TIME=10 NPAD=0 PADC=0 EOL=CR QCTL=# QBIN=N CHKT=1

def receive_transfer(link, first):
    """Server became the sender: ACK S/F/X/D/Z/B packets, collect data."""
    data = bytearray()
    seq, ptype, pdata = first
    while True:
        if ptype == "E":
            raise IOError(f"calculator error: {decode(pdata)!r}")
        if ptype in "YN" and not data:
            pass  # late ACK/NAK left over from the I exchange; ignore it
        elif ptype == "S":
            link.send(packet(seq, "Y", INIT))
        else:
            if ptype == "D":
                data += decode(pdata)
            link.send(packet(seq, "Y"))
            if ptype == "B":
                return bytes(data)
        seq, ptype, pdata = link.recv()

def main():
    host = sys.argv[1] if len(sys.argv) > 1 else "localhost"
    port = int(sys.argv[2]) if len(sys.argv) > 2 else 4848
    cmd = sys.argv[3:] or ["init"]
    link = Link(host, port)
    link.drain()
    # Resend I until it is ACKed (the server may NAK if it timed out meanwhile).
    for attempt in range(5):
        link.send(packet(0, "I", INIT))
        try:
            seq, ptype, pdata = link.recv()
        except socket.timeout:
            continue
        if ptype == "Y":
            break
        link.drain(0.3)
    else:
        sys.exit("no ACK to I packet")
    print("I-packet ACKed, calculator parameters:", pdata)
    if cmd[0] == "init":
        return
    if cmd[0] == "dir":
        link.send(packet(0, "G", b"D"))
    elif cmd[0] == "finish":
        link.send(packet(0, "G", b"F"))
        print(link.recv())
        return
    elif cmd[0] == "host":
        link.send(packet(0, "C", encode(" ".join(cmd[1:]).encode())))
    elif cmd[0] == "get":
        link.send(packet(0, "R", encode(cmd[1].encode())))
    else:
        sys.exit(f"unknown command {cmd[0]}")
    result = receive_transfer(link, link.recv())
    print("---- received", len(result), "bytes ----")
    print(result.decode("latin-1"))

if __name__ == "__main__":
    main()
