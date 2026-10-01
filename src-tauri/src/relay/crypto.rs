//! 종단간 암호 — Noise `IKpsk2_25519_ChaChaPoly_SHA256` (규격: docs/RELAY.md §2).
//! PC 는 받는 쪽(responder)이다. 알고리즘을 새로 만들지 않고 `snow` 에 맡긴다.

use snow::{HandshakeState, TransportState};

pub const NOISE_PARAMS: &str = "Noise_IKpsk2_25519_ChaChaPoly_SHA256";
pub const PROLOGUE_TAG: &[u8] = b"conoti-ai-relay/1";
pub const MODE_PAIR: u8 = 0x01;
pub const MODE_CONNECT: u8 = 0x02;
/// 조각 하나의 평문 크기(앞 1바이트 표지 제외)
pub const CHUNK: usize = 60_000;
/// 합친 앱 메시지 한계
pub const MAX_MESSAGE: usize = 4 * 1024 * 1024;

pub fn prologue(mode: u8, pid: &[u8; 16]) -> Vec<u8> {
    let mut p = PROLOGUE_TAG.to_vec();
    p.push(mode);
    p.extend_from_slice(pid);
    p
}

pub struct Keypair {
    pub private: [u8; 32],
    pub public: [u8; 32],
}

pub fn generate_keypair() -> Result<Keypair, String> {
    let kp = snow::Builder::new(NOISE_PARAMS.parse().map_err(|e| format!("{e:?}"))?)
        .generate_keypair()
        .map_err(|e| e.to_string())?;
    let mut private = [0u8; 32];
    let mut public = [0u8; 32];
    private.copy_from_slice(&kp.private);
    public.copy_from_slice(&kp.public);
    Ok(Keypair { private, public })
}

/// X25519 개인키 → 공개키 (저장해 둔 개인키만 읽었을 때)
pub fn public_of(private: &[u8; 32]) -> Result<[u8; 32], String> {
    // snow 는 공개키 계산을 따로 내놓지 않는다 — 기본 해석기의 DH 로 계산한다
    use snow::resolvers::{CryptoResolver, DefaultResolver};
    let mut dh = DefaultResolver.resolve_dh(&snow::params::DHChoice::Curve25519).ok_or("dh")?;
    dh.set(private);
    let mut out = [0u8; 32];
    out.copy_from_slice(dh.pubkey());
    Ok(out)
}

/// 폰의 첫 메시지(`mode ‖ pid ‖ noise`)를 가른다.
pub fn split_first(frame: &[u8]) -> Option<(u8, [u8; 16], &[u8])> {
    if frame.len() < 17 + 32 {
        return None;
    }
    let mode = frame[0];
    if mode != MODE_PAIR && mode != MODE_CONNECT {
        return None;
    }
    let mut pid = [0u8; 16];
    pid.copy_from_slice(&frame[1..17]);
    Some((mode, pid, &frame[17..]))
}

pub struct Responder {
    hs: HandshakeState,
}

impl Responder {
    pub fn new(local_private: &[u8; 32], psk: &[u8; 32], mode: u8, pid: &[u8; 16]) -> Result<Self, String> {
        Self::build(local_private, psk, mode, pid, None)
    }

    fn build(local_private: &[u8; 32], psk: &[u8; 32], mode: u8, pid: &[u8; 16], eph: Option<&[u8; 32]>) -> Result<Self, String> {
        let pro = prologue(mode, pid);
        let mut b = snow::Builder::new(NOISE_PARAMS.parse().map_err(|e| format!("{e:?}"))?)
            .local_private_key(local_private)
            .psk(2, psk)
            .prologue(&pro);
        if let Some(e) = eph {
            b = b.fixed_ephemeral_key_for_testing_only(e);
        }
        Ok(Self { hs: b.build_responder().map_err(|e| e.to_string())? })
    }

    /// 첫 메시지를 읽고 두 번째 메시지를 만든다. (상대 정적키, 보낼 바이트, 전송 상태)
    pub fn respond(self, msg1: &[u8]) -> Result<([u8; 32], Vec<u8>, Session), String> {
        self.respond_with_hash(msg1).map(|(r, o, s, _)| (r, o, s))
    }

    /// `respond` + 핸드셰이크 해시(페어링 확인 코드용)
    pub fn respond_with_hash(mut self, msg1: &[u8]) -> Result<([u8; 32], Vec<u8>, Session, [u8; 32]), String> {
        let mut buf = vec![0u8; 1024];
        let n = self.hs.read_message(msg1, &mut buf).map_err(|_| "handshake".to_string())?;
        if n != 0 {
            return Err("handshake payload".into());
        }
        let mut remote = [0u8; 32];
        remote.copy_from_slice(self.hs.get_remote_static().ok_or("no remote static")?);
        let mut out = vec![0u8; 1024];
        let n = self.hs.write_message(&[], &mut out).map_err(|e| e.to_string())?;
        out.truncate(n);
        let mut h = [0u8; 32];
        h.copy_from_slice(self.hs.get_handshake_hash());
        let t = self.hs.into_transport_mode().map_err(|e| e.to_string())?;
        Ok((remote, out, Session::new(t), h))
    }
}

/// 페어링 확인 코드: 양쪽 화면에 같은 6자리가 떠야 한다(QR 문자열이 새어 남이 먼저 끼어들어도 사용자가 알아챈다).
/// code = u32_be(SHA-256("conoti-ai-sas/1" ‖ h)[0..4]) % 1_000_000 → "123 456"
pub fn sas(handshake_hash: &[u8; 32]) -> String {
    use sha2::{Digest, Sha256};
    let mut d = Sha256::new();
    d.update(b"conoti-ai-sas/1");
    d.update(handshake_hash);
    let x = d.finalize();
    let n = u32::from_be_bytes([x[0], x[1], x[2], x[3]]) % 1_000_000;
    let s = format!("{n:06}");
    format!("{} {}", &s[..3], &s[3..])
}

/// 핸드셰이크 뒤의 통로. 앱 메시지를 조각내 암호화하고, 받은 조각을 이어 붙인다.
pub struct Session {
    t: TransportState,
    pending: Vec<u8>,
}

impl Session {
    fn new(t: TransportState) -> Self {
        Self { t, pending: Vec::new() }
    }

    /// 앱 메시지 하나 → 암호문 프레임 여러 개
    pub fn seal(&mut self, msg: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        if msg.len() > MAX_MESSAGE {
            return Err("message too large".into());
        }
        let mut frames = Vec::new();
        let mut chunks: Vec<&[u8]> = msg.chunks(CHUNK).collect();
        if chunks.is_empty() {
            chunks.push(&[]);
        }
        let last = chunks.len() - 1;
        for (i, c) in chunks.into_iter().enumerate() {
            let mut plain = Vec::with_capacity(c.len() + 1);
            plain.push(if i == last { 1 } else { 0 });
            plain.extend_from_slice(c);
            let mut out = vec![0u8; plain.len() + 16];
            let n = self.t.write_message(&plain, &mut out).map_err(|e| e.to_string())?;
            out.truncate(n);
            frames.push(out);
        }
        Ok(frames)
    }

    /// 암호문 프레임 하나 → 완성된 앱 메시지(아직이면 None). 실패하면 통로를 버려야 한다.
    pub fn open(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>, String> {
        if frame.len() < 17 || frame.len() > 65_535 {
            return Err("frame size".into());
        }
        let mut plain = vec![0u8; frame.len()];
        let n = self.t.read_message(frame, &mut plain).map_err(|_| "decrypt".to_string())?;
        if n == 0 {
            return Err("empty chunk".into());
        }
        let flag = plain[0];
        if flag > 1 {
            return Err("chunk flag".into());
        }
        if self.pending.len() + n - 1 > MAX_MESSAGE {
            return Err("message too large".into());
        }
        self.pending.extend_from_slice(&plain[1..n]);
        if flag == 1 {
            Ok(Some(std::mem::take(&mut self.pending)))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 폰 쪽(initiator) — 시험과 기준값 뽑기에만 쓴다.
    pub struct TestInitiator {
        pub hs: HandshakeState,
    }

    impl TestInitiator {
        pub fn new(local_private: &[u8; 32], remote_public: &[u8; 32], psk: &[u8; 32], mode: u8, pid: &[u8; 16], eph: Option<&[u8; 32]>) -> Self {
            let pro = prologue(mode, pid);
            let mut b = snow::Builder::new(NOISE_PARAMS.parse().unwrap())
                .local_private_key(local_private)
                .remote_public_key(remote_public)
                .psk(2, psk)
                .prologue(&pro);
            if let Some(e) = eph {
                b = b.fixed_ephemeral_key_for_testing_only(e);
            }
            Self { hs: b.build_initiator().unwrap() }
        }
        pub fn first(&mut self) -> Vec<u8> {
            let mut out = vec![0u8; 1024];
            let n = self.hs.write_message(&[], &mut out).unwrap();
            out.truncate(n);
            out
        }
        pub fn finish(self, msg2: &[u8]) -> Result<Session, String> {
            self.finish_with_hash(msg2).map(|(s, _)| s)
        }
        pub fn finish_with_hash(mut self, msg2: &[u8]) -> Result<(Session, [u8; 32]), String> {
            let mut buf = vec![0u8; 1024];
            self.hs.read_message(msg2, &mut buf).map_err(|_| "handshake".to_string())?;
            let mut h = [0u8; 32];
            h.copy_from_slice(self.hs.get_handshake_hash());
            Ok((Session::new(self.hs.into_transport_mode().map_err(|e| e.to_string())?), h))
        }
    }

    fn key(b: u8) -> [u8; 32] {
        let mut k = [0u8; 32];
        for (i, x) in k.iter_mut().enumerate() {
            *x = b.wrapping_add(i as u8);
        }
        k
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn roundtrip_and_chunking() {
        let desk = generate_keypair().unwrap();
        let phone = generate_keypair().unwrap();
        assert_eq!(public_of(&desk.private).unwrap(), desk.public);
        let psk = key(7);
        let pid = [3u8; 16];
        let mut ini = TestInitiator::new(&phone.private, &desk.public, &psk, MODE_CONNECT, &pid, None);
        let m1 = ini.first();
        let (remote, m2, mut ds) = Responder::new(&desk.private, &psk, MODE_CONNECT, &pid).unwrap().respond(&m1).unwrap();
        assert_eq!(remote, phone.public);
        let mut ps = ini.finish(&m2).unwrap();

        let big: Vec<u8> = (0..150_000u32).map(|i| (i % 251) as u8).collect();
        let frames = ds.seal(&big).unwrap();
        assert_eq!(frames.len(), 3);
        let mut got = None;
        for f in &frames {
            got = ps.open(f).unwrap();
        }
        assert_eq!(got.unwrap(), big);
        let back = ps.seal(b"{}").unwrap();
        assert_eq!(ds.open(&back[0]).unwrap().unwrap(), b"{}");
        // 변조된 프레임은 거절
        let mut bad = ds.seal(b"x").unwrap().remove(0);
        bad[3] ^= 1;
        assert!(ps.open(&bad).is_err());
    }

    #[test]
    fn sas_matches_both_sides_and_differs_per_handshake() {
        let desk = generate_keypair().unwrap();
        let phone = generate_keypair().unwrap();
        let z = [0u8; 16];
        let run = || {
            let mut ini = TestInitiator::new(&phone.private, &desk.public, &key(9), MODE_PAIR, &z, None);
            let m1 = ini.first();
            let (_, m2, _, dh) = Responder::new(&desk.private, &key(9), MODE_PAIR, &z).unwrap().respond_with_hash(&m1).unwrap();
            let (_, ph) = ini.finish_with_hash(&m2).unwrap();
            assert_eq!(sas(&dh), sas(&ph));
            sas(&dh)
        };
        let a = run();
        assert_eq!(a.len(), 7);
        assert_eq!(&a[3..4], " ");
        assert_ne!(a, run(), "임시 키가 다르면 코드도 달라야 한다(같을 확률 백만분의 일)");
    }

    #[test]
    fn wrong_psk_or_prologue_fails() {
        let desk = generate_keypair().unwrap();
        let phone = generate_keypair().unwrap();
        let pid = [0u8; 16];
        // psk 가 다르면 PC 는 첫 메시지는 읽지만(psk2) 폰이 두 번째 메시지에서 실패한다
        let mut ini = TestInitiator::new(&phone.private, &desk.public, &key(1), MODE_PAIR, &pid, None);
        let m1 = ini.first();
        let (_, m2, _) = Responder::new(&desk.private, &key(2), MODE_PAIR, &pid).unwrap().respond(&m1).unwrap();
        assert!(ini.finish(&m2).is_err());
        // 방식(mode)이 다르면 prologue 가 달라 PC 가 첫 메시지부터 거절
        let mut ini = TestInitiator::new(&phone.private, &desk.public, &key(1), MODE_PAIR, &pid, None);
        let m1 = ini.first();
        assert!(Responder::new(&desk.private, &key(1), MODE_CONNECT, &pid).unwrap().respond(&m1).is_err());
    }

    /// 폰 구현이 대조할 기준값. `cargo test relay_vectors -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn relay_vectors() {
        let desk_priv = key(0x10);
        let phone_priv = key(0x40);
        let phone_eph = key(0x70);
        let desk_eph = key(0xa0);
        let psk = key(0xd0);
        let pid = [0x5au8; 16];
        let desk_pub = public_of(&desk_priv).unwrap();
        let mut ini = TestInitiator::new(&phone_priv, &desk_pub, &psk, MODE_CONNECT, &pid, Some(&phone_eph));
        let m1 = ini.first();
        let (remote, m2, mut ds) = Responder::build(&desk_priv, &psk, MODE_CONNECT, &pid, Some(&desk_eph))
            .unwrap()
            .respond(&m1)
            .unwrap();
        let mut ps = ini.finish(&m2).unwrap();
        let p2d = ps.seal(br#"{"id":1,"m":"hello","p":{}}"#).unwrap();
        let d2p = ds.seal(br#"{"id":1,"ok":true,"r":{}}"#).unwrap();
        let big: Vec<u8> = (0..70_000u32).map(|i| b'a' + (i % 26) as u8).collect();
        let d2p_big = ds.seal(&big).unwrap();
        // 페어링 모드(0x01, pid 0) — 같은 키·psk 로 확인 코드
        let zero = [0u8; 16];
        let mut pini = TestInitiator::new(&phone_priv, &desk_pub, &psk, MODE_PAIR, &zero, Some(&phone_eph));
        let pm1 = pini.first();
        let (_, pm2, _, dh) = Responder::build(&desk_priv, &psk, MODE_PAIR, &zero, Some(&desk_eph)).unwrap().respond_with_hash(&pm1).unwrap();
        let (_, ph) = pini.finish_with_hash(&pm2).unwrap();
        assert_eq!(dh, ph);
        println!(
            "{}",
            serde_json::json!({
                "protocol": NOISE_PARAMS,
                "prologue_hex": hex(&prologue(MODE_CONNECT, &pid)),
                "mode": MODE_CONNECT,
                "pid_hex": hex(&pid),
                "desktop_static_private_hex": hex(&desk_priv),
                "desktop_static_public_hex": hex(&desk_pub),
                "desktop_ephemeral_private_hex": hex(&desk_eph),
                "phone_static_private_hex": hex(&phone_priv),
                "phone_static_public_hex": hex(&remote),
                "phone_ephemeral_private_hex": hex(&phone_eph),
                "psk_hex": hex(&psk),
                "msg1_hex": hex(&m1),
                "msg2_hex": hex(&m2),
                "phone_to_desktop": [{"plain": r#"{"id":1,"m":"hello","p":{}}"#, "frames_hex": p2d.iter().map(|f| hex(f)).collect::<Vec<_>>()}],
                "pair_sas": {"mode": MODE_PAIR, "pid_hex": hex(&zero), "prologue_hex": hex(&prologue(MODE_PAIR, &zero)),
                             "note": "위와 같은 정적·임시 키와 psk", "msg1_hex": hex(&pm1), "msg2_hex": hex(&pm2),
                             "handshake_hash_hex": hex(&ph), "code": sas(&ph)},
                "desktop_to_phone": [
                    {"plain": r#"{"id":1,"ok":true,"r":{}}"#, "frames_hex": d2p.iter().map(|f| hex(f)).collect::<Vec<_>>()},
                    {"plain_note": "70000 bytes: b'a' + (i % 26)", "frames_hex": d2p_big.iter().map(|f| hex(f)).collect::<Vec<_>>()}
                ]
            })
        );
    }
}
