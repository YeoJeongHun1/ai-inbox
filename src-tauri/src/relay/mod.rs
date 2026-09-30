//! 폰 연결 — 코노티 중계를 통한 종단간 암호화 통로 (규격: docs/RELAY.md).
//!
//! 켜야만 네트워크를 쓴다. 서버로 가는 것은 방 번호·암호문·푸시 티켓뿐이다.
//! 비밀값(PC 정적 개인키 · 방 비밀 · 기기별 psk)은 앱 데이터 폴더의 `relay-identity.json`(본인만 읽기 600)에 둔다.
//! 키체인을 쓰지 않는 이유: 서명 없는 앱은 업데이트마다 서명이 바뀌어 키체인 허용 창이 뜨고, 그동안 연결 스레드가 멈춘다.
//! 대화 내용이 담긴 DB 가 이미 같은 폴더·같은 보호 아래 있으므로 보호 수준은 같다.

pub mod client;
pub mod crypto;
pub mod rpc;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db;

pub const PROD_HOST: &str = "conoti.app";
pub const DEV_HOST: &str = "dev.conoti.app";
const IDENTITY_FILE: &str = "relay-identity.json";
pub const OFFER_TTL: Duration = Duration::from_secs(5 * 60);
pub const APPROVAL_TTL: Duration = Duration::from_secs(120);

pub fn host(env: &str) -> &'static str {
    if env == "dev" { DEV_HOST } else { PROD_HOST }
}

pub fn env_of(conn: &Connection) -> String {
    match db::get_meta(conn, "relay.env").as_deref() {
        Some("dev") => "dev".into(),
        _ => "prod".into(),
    }
}

pub fn enabled(conn: &Connection) -> bool {
    db::get_meta(conn, "relay.enabled").as_deref() == Some("1")
}

pub fn push_enabled(conn: &Connection) -> bool {
    db::get_meta(conn, "relay.push").as_deref() != Some("0")
}

pub fn b64(b: &[u8]) -> String {
    B64.encode(b)
}

pub fn unb64(s: &str) -> Option<Vec<u8>> {
    B64.decode(s.trim_end_matches('=')).ok()
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

pub fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("OS 난수를 얻지 못함");
    b
}

/// 방 번호 = b64url(SHA-256("conoti-relay-room/1" ‖ secret)[0..16]) — 서버와 같은 계산
pub fn room_of(secret: &[u8; 32]) -> String {
    let mut h = Sha256::new();
    h.update(b"conoti-relay-room/1");
    h.update(secret);
    b64(&h.finalize()[..16])
}

// ── 비밀값(데이터 폴더의 본인 전용 파일) ────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Default)]
struct IdentityJson {
    v: u32,
    key: String,
    secret: String,
    #[serde(default)]
    psks: HashMap<String, String>,
}

#[derive(Clone)]
pub struct Identity {
    pub key: [u8; 32],
    pub public: [u8; 32],
    pub secret: [u8; 32],
    pub psks: HashMap<String, [u8; 32]>,
}

fn identity_path() -> std::path::PathBuf {
    crate::paths::data_dir().join(IDENTITY_FILE)
}

fn arr32(v: &[u8]) -> Option<[u8; 32]> {
    v.try_into().ok()
}

impl Identity {
    /// 파일에서 읽고, 없으면 새로 만들어 저장한다.
    pub fn load_or_create() -> Result<Identity, String> {
        match std::fs::read_to_string(identity_path()) {
            Ok(raw) => {
                let j: IdentityJson = serde_json::from_str(&raw).map_err(|_| "폰 연결 정보 파일이 손상됨".to_string())?;
                let key = unb64(&j.key).and_then(|v| arr32(&v)).ok_or("키 형식 오류")?;
                let secret = unb64(&j.secret).and_then(|v| arr32(&v)).ok_or("비밀 형식 오류")?;
                let psks = j
                    .psks
                    .iter()
                    .filter_map(|(pid, p)| Some((pid.clone(), unb64(p).and_then(|v| arr32(&v))?)))
                    .collect();
                Ok(Identity { public: crypto::public_of(&key)?, key, secret, psks })
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let kp = crypto::generate_keypair()?;
                let id = Identity { key: kp.private, public: kp.public, secret: random(), psks: HashMap::new() };
                id.save()?;
                Ok(id)
            }
            Err(err) => Err(format!("폰 연결 정보를 읽지 못함: {err}")),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        if cfg!(test) {
            return Ok(()); // 시험은 실제 데이터 폴더를 건드리지 않는다
        }
        let j = IdentityJson {
            v: 1,
            key: b64(&self.key),
            secret: b64(&self.secret),
            psks: self.psks.iter().map(|(k, v)| (k.clone(), b64(v))).collect(),
        };
        let body = serde_json::to_string(&j).map_err(|e| e.to_string())?;
        let path = identity_path();
        if let Some(dir) = path.parent() {
            crate::paths::ensure_private_dir(dir);
        }
        // 임시 파일을 처음부터 600 으로 만들어 쓰고 바꿔 끼운다(쓰는 도중 남이 읽을 틈 없음)
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        {
            use std::io::Write;
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o600);
            }
            let mut f = o.open(&tmp).map_err(|e| format!("폰 연결 정보를 저장하지 못함: {e}"))?;
            f.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
            f.sync_all().ok();
        }
        crate::paths::make_private_file(&tmp);
        std::fs::rename(&tmp, &path).map_err(|e| format!("폰 연결 정보를 저장하지 못함: {e}"))
    }

    pub fn room(&self) -> String {
        room_of(&self.secret)
    }
}

// ── 기기 ─────────────────────────────────────────────────────────────────────

#[derive(Serialize, Clone, Debug)]
pub struct Device {
    pub pid: String,
    pub name: String,
    #[serde(skip)]
    pub phone_pub: String,
    pub can_reply: bool,
    /// 폰에서 세션 관리(보관함 보기·보관·고정·기록에서 지우기) 허용
    pub can_manage: bool,
    /// 폰에서 예약 전송을 만들기·고치기·취소·처리(0.10.0, 기본 끔)
    pub can_schedule: bool,
    pub created_at: String,
    pub last_seen: Option<String>,
    #[serde(skip)]
    pub ticket: Option<String>,
    #[serde(skip)]
    pub acct: Option<String>,
}

pub fn devices(conn: &Connection) -> Vec<Device> {
    let Ok(mut st) = conn.prepare(
        "SELECT pid, name, phone_pub, can_reply, created_at, last_seen, ticket, acct, can_manage, can_schedule FROM relay_device ORDER BY created_at",
    ) else {
        return vec![];
    };
    st.query_map([], |r| {
        Ok(Device {
            pid: r.get(0)?,
            name: r.get(1)?,
            phone_pub: r.get(2)?,
            can_reply: r.get::<_, i64>(3)? != 0,
            created_at: r.get(4)?,
            last_seen: r.get(5)?,
            ticket: r.get(6)?,
            acct: r.get(7)?,
            can_manage: r.get::<_, i64>(8)? != 0,
            can_schedule: r.get::<_, i64>(9)? != 0,
        })
    })
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}

pub fn device(conn: &Connection, pid: &str) -> Option<Device> {
    devices(conn).into_iter().find(|d| d.pid == pid)
}

pub fn clean_name(s: &str) -> String {
    let n: String = s.chars().filter(|c| !c.is_control()).take(40).collect::<String>().trim().to_string();
    if n.is_empty() { "이름 없는 폰".into() } else { n }
}

/// 기기를 지운다(비밀 파일의 psk 와 DB 행). 그 기기가 보낸 답 기록은 남긴다.
pub fn remove_device(conn: &Connection, shared: &Shared, pid: &str) -> Result<(), String> {
    shared.update_identity(|id| {
        id.psks.remove(pid);
    })?;
    // 이 기기가 걸어 둔 예약은 거둔다 — 연결이 해제된 폰이 만든 무인 실행이 남지 않게
    crate::sched::cancel_device(conn, pid);
    conn.execute("DELETE FROM relay_device WHERE pid = ?1", params![pid]).map_err(|e| e.to_string())?;
    shared.dropped.lock().unwrap().push(pid.to_string());
    Ok(())
}

// ── 페어링 제안 · 허용 대기 ───────────────────────────────────────────────────

pub struct Offer {
    pub s: [u8; 32],
    pub expires: Instant,
    pub used: bool,
    pub uri: String,
}

pub struct Pending {
    pub name: String,
    /// 페어링 확인 코드 — PC 창과 폰 화면에 같은 숫자가 떠야 한다
    pub sas: String,
    /// 이 요청을 한 통로(통로가 사라지면 요청도 치운다)
    pub ch: i64,
    pub requested: Instant,
    /// (허용?, 답 보내기 허용?)
    pub decision: Option<(bool, bool)>,
}

#[derive(Serialize, Clone, Default)]
pub struct Status {
    /// off · connecting · online · error
    pub state: String,
    pub error: Option<String>,
    pub since: Option<String>,
    pub phones_online: usize,
}

/// UI·수집 스레드와 중계 스레드가 함께 보는 것
pub struct Shared {
    pub status: Mutex<Status>,
    pub offer: Mutex<Option<Offer>>,
    pub pending: Mutex<Option<Pending>>,
    /// 무언가 바뀌었다(폰에 changed 알림) — 올리기만 한다
    pub changed: AtomicU64,
    /// 새 결과(푸시 후보) — 올리기만 한다
    pub finished: AtomicU64,
    /// 예약이 전달되지 못했다는 알림(푸시 후보, 0.10.0) — 올리기만 한다. 푸시는 내용 없이 종류(`k`: sched_held·sched_ready·sched_missed)만 싣는다
    pub sched_alert: AtomicU64,
    /// 그 알림들의 푸시 종류 `k`(sched::PUSH_*) — 푸시가 나갈 때 비운다
    pub sched_kinds: Mutex<Vec<&'static str>>,
    /// 설정이 바뀌었으니 다시 붙어라
    pub kick: AtomicBool,
    /// 지운 기기 — 열린 통로를 닫는다
    pub dropped: Mutex<Vec<String>>,
    /// 파일에서 한 번만 읽어 둔다
    identity: Mutex<Option<Identity>>,
}

impl Shared {
    pub fn new() -> Arc<Shared> {
        Arc::new(Shared {
            status: Mutex::new(Status { state: "off".into(), ..Default::default() }),
            offer: Mutex::new(None),
            pending: Mutex::new(None),
            changed: AtomicU64::new(0),
            finished: AtomicU64::new(0),
            sched_alert: AtomicU64::new(0),
            sched_kinds: Mutex::new(Vec::new()),
            kick: AtomicBool::new(false),
            dropped: Mutex::new(Vec::new()),
            identity: Mutex::new(None),
        })
    }

    pub fn identity(&self) -> Result<Identity, String> {
        let mut g = self.identity.lock().unwrap();
        if g.is_none() {
            *g = Some(Identity::load_or_create()?);
        }
        Ok(g.clone().unwrap())
    }

    pub fn update_identity(&self, f: impl FnOnce(&mut Identity)) -> Result<Identity, String> {
        let mut g = self.identity.lock().unwrap();
        if g.is_none() {
            *g = Some(Identity::load_or_create()?);
        }
        let id = g.as_mut().unwrap();
        f(id);
        id.save()?;
        Ok(id.clone())
    }

    #[cfg(test)]
    pub fn set_identity_for_test(&self, id: Identity) {
        *self.identity.lock().unwrap() = Some(id);
    }

    pub fn bump_changed(&self) {
        self.changed.fetch_add(1, Ordering::SeqCst);
    }

    pub fn bump_finished(&self) {
        self.finished.fetch_add(1, Ordering::SeqCst);
    }

    pub fn bump_sched_alert(&self, kinds: Vec<&'static str>) {
        if let Ok(mut k) = self.sched_kinds.lock() {
            k.extend(kinds);
        }
        self.sched_alert.fetch_add(1, Ordering::SeqCst);
    }
}

/// 새 페어링 제안을 만든다(앞 제안은 버린다). 돌려주는 값은 QR 에 담을 주소.
pub fn new_offer(shared: &Shared, env: &str, id: &Identity) -> String {
    let s: [u8; 32] = random();
    let uri = format!("conoti-ai://pair?v=1&e={env}&r={}&k={}&s={}", id.room(), b64(&id.public), b64(&s));
    *shared.offer.lock().unwrap() = Some(Offer { s, expires: Instant::now() + OFFER_TTL, used: false, uri: uri.clone() });
    uri
}

/// QR 을 SVG 로(화면이 그대로 그린다 — 외부 라이브러리·네트워크 없음)
pub fn qr_svg(data: &str) -> Result<String, String> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::with_error_correction_level(data.as_bytes(), qrcode::EcLevel::M).map_err(|e| e.to_string())?;
    Ok(code.render::<svg::Color>().min_dimensions(240, 240).quiet_zone(true).build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_matches_server_derivation() {
        // 서버 tests/test_relay.py 와 같은 계산: sha256(tag ‖ secret)[:16] → b64url(패딩 없음) 22자
        let r = room_of(&[7u8; 32]);
        assert_eq!(r, "wdZ6NJrbdduohAJEELbRqA"); // python: b64url(sha256(b'conoti-relay-room/1'+bytes([7]*32))[:16])
        assert_eq!(r, room_of(&[7u8; 32]));
        assert_ne!(r, room_of(&[8u8; 32]));
    }

    #[test]
    fn hex_roundtrip_and_names() {
        assert_eq!(unhex(&hex(&[0, 255, 16])).unwrap(), vec![0, 255, 16]);
        assert!(unhex("abc").is_none());
        assert_eq!(clean_name("  iPhone\u{0}\n 15 "), "iPhone 15");
        assert_eq!(clean_name(""), "이름 없는 폰");
    }

    #[test]
    fn qr_renders() {
        let svg = qr_svg("conoti-ai://pair?v=1&e=dev&r=AAAA&k=BBBB&s=CCCC").unwrap();
        assert!(svg.starts_with("<?xml") || svg.starts_with("<svg"));
    }
}
