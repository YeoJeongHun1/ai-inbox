#!/usr/bin/env python3
"""Tauri 업데이터 서명(.sig, minisign 형식)을 tauri.conf.json 의 공개키로 검증한다.
외부 패키지 없이 RFC 8032 Ed25519 참조 구현을 쓴다(파일 한 개 검증용이라 속도는 무관).
사용: verify-sig.py <업데이트 묶음> <.sig> <tauri.conf.json>"""
import base64, hashlib, json, sys

P = 2**255 - 19
Q = 2**252 + 27742317777372353535851937790883648493
D = -121665 * pow(121666, P - 2, P) % P
I = pow(2, (P - 1) // 4, P)


def inv(x):
    return pow(x, P - 2, P)


def xrec(y, sign):
    xx = (y * y - 1) * inv(D * y * y + 1)
    x = pow(xx, (P + 3) // 8, P)
    if (x * x - xx) % P:
        x = x * I % P
    if (x * x - xx) % P:
        raise ValueError("not on curve")
    if x & 1 != sign:
        x = P - x
    return x


GY = 4 * inv(5) % P
G = (xrec(GY, 0), GY, 1, xrec(GY, 0) * GY % P)


def add(a, b):
    A = (a[1] - a[0]) * (b[1] - b[0]) % P
    B = (a[1] + a[0]) * (b[1] + b[0]) % P
    C = 2 * a[3] * b[3] * D % P
    Dd = 2 * a[2] * b[2] % P
    E, F, Gg, H = B - A, Dd - C, Dd + C, B + A
    return (E * F % P, Gg * H % P, F * Gg % P, E * H % P)


def mul(s, pt):
    r = (0, 1, 1, 0)
    while s:
        if s & 1:
            r = add(r, pt)
        pt = add(pt, pt)
        s >>= 1
    return r


def dec(b):
    y = int.from_bytes(b, "little")
    sign, y = y >> 255, y & ((1 << 255) - 1)
    x = xrec(y, sign)
    return (x, y, 1, x * y % P)


def enc(pt):
    zi = inv(pt[2])
    x, y = pt[0] * zi % P, pt[1] * zi % P
    return (y | ((x & 1) << 255)).to_bytes(32, "little")


def verify(pub, msg, sig):
    if len(sig) != 64:
        return False
    a = dec(pub)
    r = dec(sig[:32])
    s = int.from_bytes(sig[32:], "little")
    if s >= Q:
        return False
    h = int.from_bytes(hashlib.sha512(sig[:32] + pub + msg).digest(), "little") % Q
    return enc(mul(s, G)) == enc(add(r, mul(h, a)))


def minisign_lines(text):
    return text.decode().strip().split("\n")


def main():
    pkg, sigf, conf = sys.argv[1:4]
    pub_text = base64.b64decode(json.load(open(conf))["plugins"]["updater"]["pubkey"])
    pk = base64.b64decode(minisign_lines(pub_text)[1])
    pk_id, pk_key = pk[2:10], pk[10:42]
    sg_lines = minisign_lines(base64.b64decode(open(sigf).read().strip()))
    sg = base64.b64decode(sg_lines[1])
    alg, sg_id, sig = sg[:2], sg[2:10], sg[10:74]
    if sg_id != pk_id:
        sys.exit("서명 실패: 공개키와 서명의 키 ID 가 다릅니다")
    data = open(pkg, "rb").read()
    msg = hashlib.blake2b(data).digest() if alg == b"ED" else data
    if not verify(pk_key, msg, sig):
        sys.exit("서명 실패: 파일 서명이 맞지 않습니다")
    trusted = sg_lines[2].split("trusted comment: ", 1)[1].encode()
    if not verify(pk_key, sig + trusted, base64.b64decode(sg_lines[3])):
        sys.exit("서명 실패: 전역 서명이 맞지 않습니다")
    print(f"서명 검증 통과 (키 ID {pk_id[::-1].hex().upper()}, {len(data)} 바이트)")


main()
