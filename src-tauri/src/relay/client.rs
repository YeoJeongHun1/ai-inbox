//! 중계 연결 스레드 — 코노티 중계에 방을 열고, 들어온 폰마다 Noise 통로를 맺어 RPC 에 답한다.
//! 블로킹 웹소켓 하나(0.5초 읽기 제한)로 돌고, 틈틈이 페어링 허용·변경 알림·푸시를 처리한다.

use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use super::crypto::{self, Responder, Session, MODE_CONNECT, MODE_PAIR};
use super::{b64, device, hex, host, random, rpc, unb64, unhex, Identity, Pending, Shared, APPROVAL_TTL};
use crate::{db, time};

const PING_EVERY: Duration = Duration::from_secs(25);
const SILENCE_LIMIT: Duration = Duration::from_secs(75);
const PUSH_GAP: Duration = Duration::from_secs(20);
/// "바뀜" 알림 간격 — 폰은 알림마다 목록·대화를 다시 받는다. 너무 잦으면 큰 대화에서 중계의 분당 전송 예산을 넘겨
/// 방이 끊긴다(2026-09-24 코노티 보안 재점검) → 3초에 한 번.
const CHANGED_GAP: Duration = Duration::from_millis(3000);
const RPC_PER_10S: u32 = 200;
/// 중계가 PC 연결에 거는 한도(60초에 600프레임·48MiB — 서버 `relay/hub.py`)의 3/4.
/// 최근 60초에 이만큼 보냈으면 큰 사진(`att_get`)은 잠깐 미룬다 — 한도를 넘으면 중계가 PC 를 끊어 폰이 전부 끊긴다.
const OUT_WINDOW: Duration = Duration::from_secs(60);
const OUT_FRAMES_SOFT: usize = 450;
const OUT_BYTES_SOFT: usize = 36 * 1024 * 1024;

pub struct Hooks {
    /// 새 기기가 연결을 청함(이름, 확인 코드) — 화면에 허용 창을 띄운다
    pub on_pair_request: Box<dyn Fn(&str, &str) + Send>,
    /// 폰이 읽음 표시·답 보내기로 무언가를 바꿈(세션들) — 화면·배지를 새로 고친다
    pub on_local_change: Box<dyn Fn(Vec<String>) + Send>,
}

enum Chan {
    Await { since: Instant },
    Pairing { sess: Session, remote: [u8; 32], hello: Option<(i64, Value)>, since: Instant, sas: String },
    /// confirmed: 첫 전송 메시지를 풀어 봤다(= 상대가 psk·키를 실제로 가짐). 그 전에는 온라인으로 세지 않는다
    /// greeted: 이 통로에서 hello 가 통과했다(계정 대조) — 그 전에는 다른 요청을 받지 않는다
    /// opened: 통로가 열린 때 — hello 없이 30초가 지나면 닫는다(폰 자리 점거 방지)
    Ready { sess: Session, pid: String, window: Instant, calls: u32, confirmed: bool, greeted: bool, opened: Instant },
}

const AWAIT_LIMIT: Duration = Duration::from_secs(10);

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

pub fn desktop_name() -> String {
    // 시험(시뮬레이터 촬영)에서만 — 실제 컴퓨터 이름이 화면·영상에 나오지 않게
    if cfg!(test) {
        if let Ok(n) = std::env::var("AI_INBOX_SIM_NAME") {
            return super::clean_name(&n);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(o) = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output() {
            let n = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !n.is_empty() {
                return super::clean_name(&n);
            }
        }
    }
    std::env::var("COMPUTERNAME").map(|n| super::clean_name(&n)).unwrap_or_else(|_| "PC".into())
}

/// 중계가 이 버전을 더 받지 않는다 — 폰 연결만 멈추고 업데이트를 권한다(뒤에 최소 버전)
const UPGRADE: &str = "upgrade:";

pub fn spawn(shared: Arc<Shared>, db_path: PathBuf, hooks: Hooks, version: String) {
    std::thread::Builder::new()
        .name("relay".into())
        .spawn(move || {
            let Ok(conn) = db::open(&db_path) else { return };
            let name = desktop_name();
            let mut backoff = 1u64;
            loop {
                shared.kick.store(false, Ordering::SeqCst);
                if !super::enabled(&conn) {
                    set_status(&shared, "off", None, 0);
                    std::thread::sleep(Duration::from_secs(1));
                    backoff = 1;
                    continue;
                }
                let env = super::env_of(&conn);
                let id = match shared.identity() {
                    Ok(i) => i,
                    Err(e) => {
                        set_status(&shared, "error", Some(e), 0);
                        std::thread::sleep(Duration::from_secs(15));
                        continue;
                    }
                };
                set_status(&shared, "connecting", None, 0);
                let mut run = Runner { shared: &shared, conn: &conn, hooks: &hooks, host: host(&env), env: env.clone(), name: &name, version: &version, id, chans: HashMap::new(), closing: Vec::new(), sent: VecDeque::new() };
                let started = Instant::now();
                let mut wait_more = 0u64;
                match run.session() {
                    Ok(()) => backoff = 1,
                    Err(e) if e.starts_with(UPGRADE) => {
                        let min = &e[UPGRADE.len()..];
                        let need = if min.is_empty() { "새 버전".to_string() } else { format!("{min} 이상") };
                        set_status(&shared, "upgrade", Some(format!("폰 연결을 계속 쓰려면 AI Inbox 를 {need}으로 업데이트해야 합니다")), 0);
                        // 서버 설정이 바뀔 수도 있으니 가끔 다시 본다(설정을 바꾸면 바로)
                        wait_more = 600;
                    }
                    Err(e) => {
                        diag(&format!("중계 연결 끊김: {e}"));
                        if started.elapsed() > Duration::from_secs(60) {
                            backoff = 1;
                        }
                        set_status(&shared, "error", Some(e), 0);
                    }
                }
                // 다시 붙기 전 기다림(설정을 바꿨으면 바로)
                let until = Instant::now() + Duration::from_secs(backoff.max(wait_more));
                while Instant::now() < until && !shared.kick.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(200));
                }
                backoff = (backoff * 2).min(30);
            }
        })
        .expect("relay thread");
}

/// 중계 진단 기록 — 데이터 폴더 `relay.log`(본인만 읽기). **내용은 적지 않는다**: 시각·통로·요청 이름·오류 코드/종류만.
/// 폰에서 "올리는 중"에 멈췄는데 PC 에 기록이 없던 일(09-25)을 다음에는 볼 수 있게. 256KB 를 넘으면 뒤쪽 절반만 남긴다.
pub(crate) fn diag(line: &str) {
    use std::io::Write;
    if cfg!(test) {
        return; // 시험이 실제 데이터 폴더에 쓰지 않게
    }
    let path = crate::paths::data_dir().join("relay.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 256 * 1024 {
            if let Ok(old) = std::fs::read(&path) {
                let keep = &old[old.len() / 2..];
                let start = keep.iter().position(|b| *b == b'\n').map(|i| i + 1).unwrap_or(0);
                let _ = std::fs::write(&path, &keep[start..]);
            }
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        crate::paths::make_private_file(&path);
        // 한 줄·200자 — 오류 문구에 섞인 줄바꿈으로 가짜 줄을 끼우거나 파일을 불리지 못하게
        let clean: String = line.chars().map(|c| if c.is_control() { ' ' } else { c }).take(200).collect();
        let _ = writeln!(f, "{} {clean}", time::now_iso());
    }
}

fn set_status(shared: &Shared, state: &str, error: Option<String>, phones: usize) {
    let mut s = shared.status.lock().unwrap();
    if s.state != state {
        s.since = Some(time::now_iso());
    }
    s.state = state.to_string();
    if state != "error" || error.is_some() {
        s.error = error;
    }
    s.phones_online = phones;
}

struct Runner<'a> {
    shared: &'a Shared,
    conn: &'a Connection,
    hooks: &'a Hooks,
    host: &'static str,
    env: String,
    name: &'a str,
    version: &'a str,
    id: Identity,
    chans: HashMap<i64, Chan>,
    /// 최근 60초에 보낸 프레임(보낸 때, 바이트) — [OUT_FRAMES_SOFT]
    sent: VecDeque<(Instant, usize)>,
    /// 답을 보낸 뒤 닫을 통로
    closing: Vec<i64>,
}

fn is_timeout(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(io) if matches!(io.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut))
}

impl<'a> Runner<'a> {
    fn connect(&self) -> Result<Ws, String> {
        let addr = (self.host, 443)
            .to_socket_addrs()
            .map_err(|e| format!("주소를 찾지 못함: {e}"))?
            .next()
            .ok_or("주소 없음")?;
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10)).map_err(|e| format!("연결 실패: {e}"))?;
        tcp.set_read_timeout(Some(Duration::from_secs(15))).ok();
        tcp.set_write_timeout(Some(Duration::from_secs(15))).ok();
        tcp.set_nodelay(true).ok();
        let knob = tcp.try_clone().map_err(|e| e.to_string())?;
        let mut cfg = WebSocketConfig::default();
        cfg.max_message_size = Some(2 * 1024 * 1024);
        cfg.max_frame_size = Some(2 * 1024 * 1024);
        let url = format!("wss://{}/v1/relay/ws", self.host);
        let (ws, _) = tungstenite::client_tls_with_config(url.as_str(), tcp, Some(cfg), None).map_err(|e| format!("웹소켓 연결 실패: {e}"))?;
        // 핸드셰이크가 끝났으니 짧게 기다리며 돌 수 있게
        knob.set_read_timeout(Some(Duration::from_millis(500))).ok();
        Ok(ws)
    }

    fn send(ws: &mut Ws, v: Value) -> Result<(), String> {
        ws.send(Message::Text(v.to_string())).map_err(|e| format!("보내기 실패: {e}"))
    }

    fn session(&mut self) -> Result<(), String> {
        let mut ws = self.connect()?;
        // app: 중계가 옛 버전을 받지 않을 때(bye: upgrade) 판단하는 데 쓴다 — 내용과 무관한 앱 버전뿐
        Self::send(&mut ws, json!({"t": "host", "v": 1, "secret": b64(&self.id.secret), "app": self.version}))?;
        let mut last_rx = Instant::now();
        let mut last_ping = Instant::now();
        let mut ready = false;
        let mut seen_changed = self.shared.changed.load(Ordering::SeqCst);
        let mut sent_changed_at = Instant::now() - CHANGED_GAP;
        let mut seen_finished = self.shared.finished.load(Ordering::SeqCst);
        let mut push_due = false;
        let mut last_push = Instant::now() - PUSH_GAP;
        loop {
            if self.shared.kick.load(Ordering::SeqCst) || !super::enabled(self.conn) || super::env_of(self.conn) != self.env {
                let _ = ws.close(None);
                return Ok(());
            }
            match ws.read() {
                Ok(Message::Text(t)) => {
                    last_rx = Instant::now();
                    let v: Value = serde_json::from_str(&t).map_err(|_| "서버 프레임 형식 오류".to_string())?;
                    match v["t"].as_str().unwrap_or("") {
                        "ready" => {
                            ready = true;
                            set_status(self.shared, "online", None, self.phones());
                        }
                        "open" => {
                            if let Some(ch) = v["ch"].as_i64() {
                                self.chans.insert(ch, Chan::Await { since: Instant::now() });
                            }
                        }
                        "close" => {
                            if let Some(ch) = v["ch"].as_i64() {
                                diag(&format!("ch={ch} 폰이 닫음"));
                                self.chans.remove(&ch);
                                set_status(self.shared, "online", None, self.phones());
                            }
                        }
                        "d" => {
                            let (Some(ch), Some(d)) = (v["ch"].as_i64(), v["d"].as_str()) else { continue };
                            let out = match unb64(d) {
                                Some(bytes) => self.on_data(ch, &bytes),
                                None => Err("형식".into()),
                            };
                            match out {
                                Ok(frames) => {
                                    for f in frames {
                                        self.send_d(&mut ws, ch, &f)?;
                                    }
                                    for c in std::mem::take(&mut self.closing) {
                                        self.chans.remove(&c);
                                        Self::send(&mut ws, json!({"t": "close", "ch": c}))?;
                                    }
                                }
                                Err(e) => {
                                    diag(&format!("ch={ch} 통로 오류로 닫음: {e}"));
                                    self.chans.remove(&ch);
                                    Self::send(&mut ws, json!({"t": "close", "ch": ch}))?;
                                }
                            }
                            set_status(self.shared, "online", None, self.phones());
                        }
                        "pong" => {}
                        "bye" if v["code"].as_str() == Some("upgrade") => {
                            return Err(format!("{UPGRADE}{}", v["min"].as_str().unwrap_or("")));
                        }
                        "bye" => return Err(format!("중계가 연결을 끝냄: {}", v["code"].as_str().unwrap_or("?"))),
                        _ => {}
                    }
                }
                Ok(Message::Close(_)) => return Err("중계가 연결을 닫음".into()),
                Ok(_) => {}
                Err(e) if is_timeout(&e) => {}
                Err(e) => return Err(format!("연결 끊김: {e}")),
            }

            // ── 틈틈이 할 일 ──
            if last_rx.elapsed() > SILENCE_LIMIT {
                return Err("중계 응답 없음".into());
            }
            if last_ping.elapsed() > PING_EVERY {
                Self::send(&mut ws, json!({"t": "ping"}))?;
                last_ping = Instant::now();
            }
            if !ready {
                continue;
            }
            // 페어링 허용/거절
            let frames = self.pairing_tick();
            for (ch, fs, close) in frames {
                for f in fs {
                    self.send_d(&mut ws, ch, &f)?;
                }
                if close {
                    self.chans.remove(&ch);
                    Self::send(&mut ws, json!({"t": "close", "ch": ch}))?;
                }
            }
            // 지운 기기의 통로 닫기
            let dropped: Vec<String> = std::mem::take(&mut *self.shared.dropped.lock().unwrap());
            if !dropped.is_empty() {
                let chs: Vec<i64> = self
                    .chans
                    .iter()
                    .filter(|(_, c)| matches!(c, Chan::Ready { pid, .. } if dropped.contains(pid)))
                    .map(|(ch, _)| *ch)
                    .collect();
                for ch in chs {
                    self.chans.remove(&ch);
                    Self::send(&mut ws, json!({"t": "close", "ch": ch}))?;
                }
                if let Ok(id) = self.shared.identity() {
                    self.id = id;
                }
            }
            // 바뀜 알림
            let changed = self.shared.changed.load(Ordering::SeqCst);
            if changed != seen_changed && sent_changed_at.elapsed() >= CHANGED_GAP {
                seen_changed = changed;
                sent_changed_at = Instant::now();
                let msg = json!({"ev": "changed"}).to_string();
                let mut out = Vec::new();
                for (ch, c) in self.chans.iter_mut() {
                    if let Chan::Ready { sess, .. } = c {
                        if let Ok(fs) = sess.seal(msg.as_bytes()) {
                            out.push((*ch, fs));
                        }
                    }
                }
                for (ch, fs) in out {
                    for f in fs {
                        Self::send(&mut ws, json!({"t": "d", "ch": ch, "d": b64(&f)}))?;
                    }
                }
            }
            // 푸시 — 폰 앱이 열려 있지 않을 때만, 20초에 한 번까지
            let finished = self.shared.finished.load(Ordering::SeqCst);
            if finished != seen_finished {
                seen_finished = finished;
                push_due = true;
            }
            if push_due && last_push.elapsed() >= PUSH_GAP {
                push_due = false;
                if self.phones() == 0 && super::push_enabled(self.conn) {
                    last_push = Instant::now();
                    self.push();
                }
            }
        }
    }

    /// 암호문 한 프레임을 보내고 전송량에 적는다.
    fn send_d(&mut self, ws: &mut Ws, ch: i64, f: &[u8]) -> Result<(), String> {
        let text = json!({"t": "d", "ch": ch, "d": b64(f)}).to_string();
        let n = text.len();
        ws.send(Message::Text(text)).map_err(|e| format!("보내기 실패: {e}"))?;
        self.sent.push_back((Instant::now(), n));
        Ok(())
    }

    /// 최근 60초 전송량이 중계 한도의 3/4 을 넘었나
    fn out_busy(&mut self) -> bool {
        while self.sent.front().is_some_and(|(t, _)| t.elapsed() > OUT_WINDOW) {
            self.sent.pop_front();
        }
        self.sent.len() >= OUT_FRAMES_SOFT || self.sent.iter().map(|(_, n)| n).sum::<usize>() >= OUT_BYTES_SOFT
    }

    fn phones(&self) -> usize {
        // hello(계정 대조)까지 통과한 통로만 — 말 없는 통로가 "폰이 보고 있음"으로 세여 푸시를 막지 않게
        self.chans.values().filter(|c| matches!(c, Chan::Ready { confirmed: true, greeted: true, .. })).count()
    }

    /// 암호문 하나 → 보낼 암호문들. Err 면 통로를 닫는다.
    fn on_data(&mut self, ch: i64, bytes: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        let state = self.chans.remove(&ch).ok_or("모르는 통로")?;
        match state {
            Chan::Await { .. } => {
                let (mode, pid, msg1) = crypto::split_first(bytes).ok_or("첫 메시지 형식")?;
                if mode == MODE_PAIR {
                    let s = {
                        let offer = self.shared.offer.lock().unwrap();
                        match offer.as_ref() {
                            Some(o) if !o.used && Instant::now() < o.expires && pid == [0u8; 16] => o.s,
                            _ => return Err("페어링 제안 없음".into()),
                        }
                    };
                    let (remote, out, sess, h) = Responder::new(&self.id.key, &s, MODE_PAIR, &pid)?.respond_with_hash(msg1)?;
                    self.chans.insert(ch, Chan::Pairing { sess, remote, hello: None, since: Instant::now(), sas: crypto::sas(&h) });
                    Ok(vec![out])
                } else if mode == MODE_CONNECT {
                    let pid_hex = hex(&pid);
                    let dev = device(self.conn, &pid_hex).ok_or("모르는 기기")?;
                    let psk = *self.id.psks.get(&pid_hex).ok_or("저장된 기기 비밀이 없음")?;
                    let (remote, out, sess) = Responder::new(&self.id.key, &psk, MODE_CONNECT, &pid)?.respond(msg1)?;
                    if unhex(&dev.phone_pub).as_deref() != Some(&remote[..]) {
                        return Err("기기 키가 다름".into());
                    }
                    self.chans.insert(ch, Chan::Ready { sess, pid: pid_hex, window: Instant::now(), calls: 0, confirmed: false, greeted: false, opened: Instant::now() });
                    Ok(vec![out])
                } else {
                    Err("방식".into())
                }
            }
            Chan::Pairing { mut sess, remote, hello, since, sas } => {
                let msg = sess.open(bytes)?;
                let Some(msg) = msg else {
                    self.chans.insert(ch, Chan::Pairing { sess, remote, hello, since, sas });
                    return Ok(vec![]);
                };
                if hello.is_some() {
                    return Err("허용 전에는 hello 한 번만".into());
                }
                let req: Value = serde_json::from_slice(&msg).map_err(|_| "JSON")?;
                if req["m"] != "hello" {
                    return Err("페어링은 hello 로 시작".into());
                }
                let rid = req["id"].as_i64().unwrap_or(0);
                let name = super::clean_name(req["p"]["name"].as_str().unwrap_or(""));
                // psk 를 아는 폰이 왔다 — 이 제안은 다시 못 쓴다
                if let Some(o) = self.shared.offer.lock().unwrap().as_mut() {
                    o.used = true;
                }
                {
                    let mut pending = self.shared.pending.lock().unwrap();
                    if pending.is_some() {
                        let frames = sess.seal(json!({"id": rid, "ok": false, "err": "busy", "msg": "다른 기기가 허용을 기다리는 중"}).to_string().as_bytes())?;
                        self.closing.push(ch);
                        return Ok(frames);
                    }
                    *pending = Some(Pending { name: name.clone(), sas: sas.clone(), ch, requested: Instant::now(), decision: None });
                }
                (self.hooks.on_pair_request)(&name, &sas);
                self.chans.insert(ch, Chan::Pairing { sess, remote, hello: Some((rid, req["p"].clone())), since, sas });
                Ok(vec![])
            }
            Chan::Ready { mut sess, pid, mut window, mut calls, confirmed, mut greeted, opened } => {
                let Some(msg) = sess.open(bytes)? else {
                    self.chans.insert(ch, Chan::Ready { sess, pid, window, calls, confirmed: true, greeted, opened });
                    return Ok(vec![]);
                };
                if !confirmed {
                    let _ = self.conn.execute("UPDATE relay_device SET last_seen = ?2 WHERE pid = ?1", params![pid, time::now_iso()]);
                }
                if window.elapsed() > Duration::from_secs(10) {
                    window = Instant::now();
                    calls = 0;
                }
                calls += 1;
                if calls > RPC_PER_10S {
                    return Err("요청이 너무 많음".into());
                }
                let req: Value = serde_json::from_slice(&msg).map_err(|_| "JSON")?;
                let is_hello = req["m"] == "hello";
                let (resp, close) = if !greeted && !is_hello {
                    // 계정 대조(hello)를 거치지 않은 통로는 아무것도 받지 않는다
                    (json!({"id": req["id"], "ok": false, "err": "hello_required", "msg": "먼저 hello"}), false)
                } else {
                    self.call(&pid, &req)
                };
                if is_hello && resp["ok"] == true {
                    greeted = true;
                }
                let frames = sess.seal(resp.to_string().as_bytes())?;
                if !close {
                    self.chans.insert(ch, Chan::Ready { sess, pid, window, calls, confirmed: true, greeted, opened });
                } else {
                    self.closing.push(ch);
                }
                Ok(frames)
            }
        }
    }

    /// 허용 대기 중인 통로에 결정이 났거나 시간이 지났으면 답을 만든다. (통로, 보낼 것, 닫을지)
    fn pairing_tick(&mut self) -> Vec<(i64, Vec<Vec<u8>>, bool)> {
        let mut out = Vec::new();
        let waiting: Vec<i64> = self
            .chans
            .iter()
            .filter(|(_, c)| matches!(c, Chan::Pairing { hello: Some(_), .. }))
            .map(|(ch, _)| *ch)
            .collect();
        // 핸드셰이크를 안 하는 통로(10초)·핸드셰이크만 하고 말이 없는 통로(30초)는 닫는다 — 폰 자리 점거 방지
        let stale: Vec<i64> = self
            .chans
            .iter()
            .filter(|(_, c)| match c {
                Chan::Await { since } => since.elapsed() > AWAIT_LIMIT,
                Chan::Pairing { hello: None, since, .. } => since.elapsed() > Duration::from_secs(30),
                Chan::Ready { greeted: false, opened, .. } => opened.elapsed() > Duration::from_secs(30),
                _ => false,
            })
            .map(|(ch, _)| *ch)
            .collect();
        for ch in stale {
            self.chans.remove(&ch);
            out.push((ch, vec![], true));
        }
        if waiting.is_empty() {
            // 허용을 기다리던 통로가 사라졌으면(폰이 나감·끊김) 요청도 치운다 — 안 그러면 다음 페어링이 계속 busy 다
            let mut p = self.shared.pending.lock().unwrap();
            if p.is_some() {
                *p = None;
            }
            return out;
        }
        let (decision, decided_ch) = {
            let mut p = self.shared.pending.lock().unwrap();
            let ch = p.as_ref().map(|pd| pd.ch);
            let d = match p.as_ref() {
                Some(pd) if !waiting.contains(&pd.ch) => Some((false, false)), // 요청한 통로가 없다
                Some(pd) if pd.decision.is_some() => pd.decision,
                Some(pd) if pd.requested.elapsed() > APPROVAL_TTL => Some((false, false)),
                None => Some((false, false)),
                _ => None,
            };
            if d.is_some() {
                *p = None;
            }
            (d, ch)
        };
        let Some((approve, can_reply)) = decision else { return out };
        for ch in waiting {
            let Some(Chan::Pairing { mut sess, remote, hello: Some((rid, p)), .. }) = self.chans.remove(&ch) else { continue };
            let approve = approve && decided_ch == Some(ch);
            if !approve {
                let f = sess.seal(json!({"id": rid, "ok": false, "err": "denied", "msg": "PC 에서 거절했거나 시간이 지났습니다"}).to_string().as_bytes()).unwrap_or_default();
                out.push((ch, f, true));
                continue;
            }
            match self.add_device(&remote, &p, can_reply) {
                Ok((pid, psk)) => {
                    let resp = json!({"id": rid, "ok": true, "r": {
                        "desktop": self.name, "version": self.version, "can_reply": can_reply, "can_manage": can_reply,
                        "paired": {"pid": b64(&pid), "psk": b64(&psk), "room": self.id.room()}
                    }});
                    let f = sess.seal(resp.to_string().as_bytes()).unwrap_or_default();
                    // 페어링 hello 가 곧 계정을 정한 hello 다(add_device 가 acct 를 기록)
                    self.chans.insert(ch, Chan::Ready { sess, pid: hex(&pid), window: Instant::now(), calls: 0, confirmed: true, greeted: true, opened: Instant::now() });
                    out.push((ch, f, false));
                    (self.hooks.on_local_change)(vec![]);
                }
                Err(e) => {
                    let f = sess.seal(json!({"id": rid, "ok": false, "err": "internal", "msg": e}).to_string().as_bytes()).unwrap_or_default();
                    out.push((ch, f, true));
                }
            }
        }
        out
    }

    fn add_device(&mut self, remote: &[u8; 32], p: &Value, can_reply: bool) -> Result<([u8; 16], [u8; 32]), String> {
        let pid: [u8; 16] = random();
        let psk: [u8; 32] = random();
        let pid_hex = hex(&pid);
        self.id = self.shared.update_identity(|id| {
            id.psks.insert(pid_hex.clone(), psk);
        })?;
        let now = time::now_iso();
        self.conn
            .execute(
                // 기록 관리 허용은 페어링 때 고른 답 보내기 허용을 따른다(설정에서 따로 바꿀 수 있다)
                "INSERT INTO relay_device (pid, name, phone_pub, can_reply, can_manage, created_at, last_seen, ticket, acct) VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?5, ?6, ?7)",
                params![pid_hex, super::clean_name(p["name"].as_str().unwrap_or("")), hex(remote), can_reply as i64, now, ticket_of(p), acct_of(p)],
            )
            .map_err(|e| e.to_string())?;
        Ok((pid, psk))
    }

    /// RPC 하나 → (응답, 통로를 닫을지)
    fn call(&mut self, pid: &str, req: &Value) -> (Value, bool) {
        let id = req["id"].clone();
        let p = &req["p"];
        let Some(dev) = device(self.conn, pid) else {
            return (json!({"id": id, "ok": false, "err": "unpaired", "msg": "PC 에서 연결이 해제됐습니다"}), true);
        };
        let caller = rpc::Caller { pid, can_reply: dev.can_reply, can_manage: dev.can_manage };
        let res: rpc::RpcResult = match req["m"].as_str().unwrap_or("") {
            "hello" => {
                // 기기는 처음 연결한 코노티 계정에 묶인다 — 같은 폰에서 다른 계정으로 로그인하면 이 PC 를 볼 수 없다
                // (계정 원문이 아니라 해시 acct 로 대조한다. 계정 없이 페어링한 옛 기기는 처음 온 계정에 묶인다)
                if let Some(bound) = dev.acct.as_deref() {
                    if acct_of(p).as_deref() != Some(bound) {
                        diag(&format!("{pid:.8} hello 거절: 다른 계정"));
                        return (
                            json!({"id": id, "ok": false, "err": "account", "msg": "이 PC 는 다른 코노티 계정에 연결돼 있습니다"}),
                            true,
                        );
                    }
                }
                let _ = self.conn.execute(
                    "UPDATE relay_device SET last_seen = ?2, ticket = COALESCE(?3, ticket), acct = COALESCE(?4, acct) WHERE pid = ?1",
                    params![pid, time::now_iso(), ticket_of(p), acct_of(p)],
                );
                Ok(json!({"desktop": self.name, "version": self.version, "can_reply": dev.can_reply, "can_manage": dev.can_manage}))
            }
            "sessions" => rpc::sessions(self.conn, p, &caller),
            "chat" => rpc::chat(self.conn, p, &caller),
            "turn" => rpc::turn(self.conn, p, &caller),
            "read" => rpc::read(self.conn, p, &caller).map(|n| {
                if n > 0 {
                    (self.hooks.on_local_change)(vec![]);
                    self.shared.bump_changed();
                }
                json!({})
            }),
            "reply" => {
                let r = rpc::reply(self.conn, p, &caller);
                if r.is_ok() {
                    (self.hooks.on_local_change)(p["sid"].as_str().map(|s| vec![s.to_string()]).unwrap_or_default());
                }
                r
            }
            "att" => rpc::att(self.conn, p, &caller),
            "att_get" if self.out_busy() => Err(("busy", "PC 가 방금 많이 보냈어요. 잠시 뒤 다시 열어 주세요".into())),
            "att_get" => rpc::att_get(self.conn, p, &caller),
            "manage" | "tidy" => {
                let r = if req["m"] == "manage" { rpc::manage(self.conn, p, &caller) } else { rpc::tidy(self.conn, p, &caller) };
                if r.is_ok() {
                    // PC 화면 목록·배지와 다른 폰들에 알린다
                    (self.hooks.on_local_change)(vec![]);
                    self.shared.bump_changed();
                }
                r
            }
            "unpair" => {
                let r = super::remove_device(self.conn, self.shared, pid).map(|_| json!({})).map_err(|e| ("internal", e));
                if r.is_ok() {
                    if let Ok(i) = self.shared.identity() {
                        self.id = i;
                    }
                    (self.hooks.on_local_change)(vec![]);
                    return (json!({"id": id, "ok": true, "r": {}}), true);
                }
                r
            }
            _ => Err(("unknown_method", "모르는 요청".into())),
        };
        // 진단 기록에는 아는 이름만 — 폰이 보낸 글을 그대로 적지 않는다(줄 끼워 넣기·파일 불리기)
        let m = match req["m"].as_str().unwrap_or("") {
            m @ ("hello" | "sessions" | "chat" | "turn" | "read" | "reply" | "att" | "att_get" | "manage" | "tidy" | "unpair") => m,
            _ => "?",
        };
        match res {
            Ok(r) => {
                if m == "att" {
                    diag(&format!("{pid:.8} att 받음 {}B", r["bytes"].as_i64().unwrap_or(0)));
                }
                (json!({"id": id, "ok": true, "r": r}), false)
            }
            Err((code, msg)) => {
                diag(&format!("{pid:.8} {m} 실패 {code}: {msg}"));
                (json!({"id": id, "ok": false, "err": code, "msg": msg}), false)
            }
        }
    }

    /// 새 결과 알림 — 계정마다 가장 최근 티켓 하나로. 내용은 싣지 않는다(서버가 고정 문구를 보낸다).
    fn push(&self) {
        let mut by_acct: HashMap<String, (String, String)> = HashMap::new(); // acct → (last_seen, ticket)
        for d in super::devices(self.conn) {
            let Some(t) = d.ticket.clone() else { continue };
            let key = d.acct.clone().unwrap_or_else(|| d.pid.clone());
            let seen = d.last_seen.clone().unwrap_or_default();
            match by_acct.get(&key) {
                Some((s, _)) if *s >= seen => {}
                _ => {
                    by_acct.insert(key, (seen, t));
                }
            }
        }
        if by_acct.is_empty() {
            return;
        }
        let url = format!("https://{}/v1/relay/push", self.host);
        let tickets: Vec<String> = by_acct.into_values().map(|(_, t)| t).collect();
        std::thread::spawn(move || {
            let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(15)).redirects(0).build();
            for t in tickets {
                let _ = agent.post(&url).send_json(json!({"ticket": t}));
            }
        });
    }
}

fn ticket_of(p: &Value) -> Option<String> {
    p["ticket"].as_str().filter(|t| t.starts_with("pt1.") && t.len() <= 512 && t.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))).map(str::to_string)
}

fn acct_of(p: &Value) -> Option<String> {
    p["acct"].as_str().filter(|a| a.len() == 16 && a.chars().all(|c| c.is_ascii_hexdigit())).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::crypto::tests::TestInitiator;
    use crate::relay::crypto::generate_keypair;

    fn setup() -> (Arc<Shared>, Connection, Identity) {
        let c = Connection::open_in_memory().unwrap();
        db::migrate(&c).unwrap();
        let kp = generate_keypair().unwrap();
        let id = Identity { key: kp.private, public: kp.public, secret: [9u8; 32], psks: HashMap::new() };
        let shared = Shared::new();
        shared.set_identity_for_test(id.clone());
        (shared, c, id)
    }

    fn first(mode: u8, pid: &[u8; 16], m1: &[u8]) -> Vec<u8> {
        let mut f = vec![mode];
        f.extend_from_slice(pid);
        f.extend_from_slice(m1);
        f
    }

    /// 페어링 → 허용 → 같은 통로로 RPC → 연결 모드로 다시 붙기까지 (네트워크 없이 Runner 만)
    #[test]
    fn pair_approve_then_connect() {
        let (shared, conn, id) = setup();
        let hooks = Hooks { on_pair_request: Box::new(|_, _| {}), on_local_change: Box::new(|_| {}) };
        let mut r = Runner { shared: &shared, conn: &conn, hooks: &hooks, host: "dev.conoti.app", env: "dev".into(), name: "PC", version: "t", id: id.clone(), chans: HashMap::new(), closing: Vec::new(), sent: VecDeque::new() };
        let uri = super::super::new_offer(&shared, "dev", &id);
        let s: [u8; 32] = unb64(uri.split("&s=").nth(1).unwrap()).unwrap().try_into().unwrap();
        let phone = generate_keypair().unwrap();

        // 핸드셰이크
        r.chans.insert(1, Chan::Await { since: Instant::now() });
        let mut ini = TestInitiator::new(&phone.private, &id.public, &s, MODE_PAIR, &[0u8; 16], None);
        let m2 = r.on_data(1, &first(MODE_PAIR, &[0u8; 16], &ini.first())).unwrap();
        let mut ps = ini.finish(&m2[0]).unwrap();
        // hello → 허용 대기
        let hello = ps.seal(br#"{"id":1,"m":"hello","p":{"name":"iPhone","ticket":"pt1.abc","acct":"0123456789abcdef"}}"#).unwrap();
        assert!(r.on_data(1, &hello[0]).unwrap().is_empty());
        assert!(shared.offer.lock().unwrap().as_ref().unwrap().used);
        assert!(r.pairing_tick().is_empty());
        shared.pending.lock().unwrap().as_mut().unwrap().decision = Some((true, false));
        let out = r.pairing_tick();
        let resp: Value = serde_json::from_slice(&ps.open(&out[0].1[0]).unwrap().unwrap()).unwrap();
        assert_eq!(resp["ok"], true);
        let pid: [u8; 16] = unb64(resp["r"]["paired"]["pid"].as_str().unwrap()).unwrap().try_into().unwrap();
        let psk: [u8; 32] = unb64(resp["r"]["paired"]["psk"].as_str().unwrap()).unwrap().try_into().unwrap();
        assert_eq!(resp["r"]["paired"]["room"], id.room());
        let dev = device(&conn, &hex(&pid)).unwrap();
        assert_eq!(dev.name, "iPhone");
        assert!(!dev.can_reply);
        assert_eq!(dev.acct.as_deref(), Some("0123456789abcdef"));

        // 같은 통로에서 바로 RPC
        let q = ps.seal(br#"{"id":2,"m":"sessions","p":{}}"#).unwrap();
        let a = r.on_data(1, &q[0]).unwrap();
        let resp: Value = serde_json::from_slice(&ps.open(&a[0]).unwrap().unwrap()).unwrap();
        assert_eq!(resp["id"], 2);
        assert_eq!(resp["ok"], true);

        // 한 번 쓴 제안으로 다시 페어링 → 거절
        r.chans.insert(2, Chan::Await { since: Instant::now() });
        let mut ini = TestInitiator::new(&phone.private, &id.public, &s, MODE_PAIR, &[0u8; 16], None);
        assert!(r.on_data(2, &first(MODE_PAIR, &[0u8; 16], &ini.first())).is_err());

        // 연결 모드: 저장된 psk 로
        r.chans.insert(3, Chan::Await { since: Instant::now() });
        let mut ini = TestInitiator::new(&phone.private, &id.public, &psk, MODE_CONNECT, &pid, None);
        let m2 = r.on_data(3, &first(MODE_CONNECT, &pid, &ini.first())).unwrap();
        let mut ps3 = ini.finish(&m2[0]).unwrap();
        // hello(계정 대조) 전에는 아무것도 받지 않는다
        let base = r.phones(); // 페어링한 통로(1번)는 이미 인사를 마쳤다
        let q = ps3.seal(br#"{"id":1,"m":"sessions","p":{}}"#).unwrap();
        let resp: Value = serde_json::from_slice(&ps3.open(&r.on_data(3, &q[0]).unwrap()[0]).unwrap().unwrap()).unwrap();
        assert_eq!(resp["err"], "hello_required");
        assert_eq!(r.phones(), base, "hello 전 통로는 '폰이 보는 중'으로 세지 않는다(푸시를 막지 않게)");
        let q = ps3.seal(br#"{"id":2,"m":"hello","p":{"name":"iPhone","acct":"0123456789abcdef"}}"#).unwrap();
        let resp: Value = serde_json::from_slice(&ps3.open(&r.on_data(3, &q[0]).unwrap()[0]).unwrap().unwrap()).unwrap();
        assert_eq!(resp["r"]["can_reply"], false);
        assert_eq!(r.phones(), base + 1);

        // hello 없이 말만 거는 통로는 30초 뒤 닫는다(폰 자리 점거 방지)
        r.chans.insert(7, Chan::Await { since: Instant::now() });
        let mut ini = TestInitiator::new(&phone.private, &id.public, &psk, MODE_CONNECT, &pid, None);
        let m2 = r.on_data(7, &first(MODE_CONNECT, &pid, &ini.first())).unwrap();
        let mut ps7 = ini.finish(&m2[0]).unwrap();
        let q = ps7.seal(br#"{"id":1,"m":"sessions","p":{}}"#).unwrap();
        r.on_data(7, &q[0]).unwrap();
        assert!(r.pairing_tick().iter().all(|(ch, _, _)| *ch != 7), "30초 전에는 두고");
        if let Some(Chan::Ready { opened, .. }) = r.chans.get_mut(&7) {
            *opened = Instant::now() - Duration::from_secs(31);
        }
        assert!(r.pairing_tick().iter().any(|(ch, _, close)| *ch == 7 && *close));
        assert!(!r.chans.contains_key(&7));
        assert_eq!(r.phones(), base + 1);

        // 최근 60초 전송량이 중계 한도에 가까우면 큰 사진(att_get)은 미룬다 — 한도를 넘으면 중계가 PC 를 끊는다
        for _ in 0..OUT_FRAMES_SOFT {
            r.sent.push_back((Instant::now(), 100));
        }
        let q = ps3.seal(br#"{"id":4,"m":"att_get","p":{"id":"0000000000000000000000000000000000000000000000000000000000000000"}}"#).unwrap();
        let resp: Value = serde_json::from_slice(&ps3.open(&r.on_data(3, &q[0]).unwrap()[0]).unwrap().unwrap()).unwrap();
        assert_eq!(resp["err"], "busy", "{resp}");
        r.sent.clear();
        r.sent.push_back((Instant::now() - Duration::from_secs(61), OUT_BYTES_SOFT));
        assert!(!r.out_busy(), "60초가 지난 전송은 세지 않는다");
        let q = ps3.seal(br#"{"id":3,"m":"sessions","p":{}}"#).unwrap();
        let resp: Value = serde_json::from_slice(&ps3.open(&r.on_data(3, &q[0]).unwrap()[0]).unwrap().unwrap()).unwrap();
        assert_eq!(resp["ok"], true);

        // 같은 폰(같은 pid·psk)이라도 다른 코노티 계정으로는 못 본다 — 계정 없이 와도 마찬가지 · 통로를 닫는다
        for (ch, hello) in [
            (5i64, br#"{"id":1,"m":"hello","p":{"name":"iPhone","acct":"fedcba9876543210"}}"#.to_vec()),
            (6i64, br#"{"id":1,"m":"hello","p":{"name":"iPhone"}}"#.to_vec()),
        ] {
            r.chans.insert(ch, Chan::Await { since: Instant::now() });
            let mut ini = TestInitiator::new(&phone.private, &id.public, &psk, MODE_CONNECT, &pid, None);
            let m2 = r.on_data(ch, &first(MODE_CONNECT, &pid, &ini.first())).unwrap();
            let mut ps5 = ini.finish(&m2[0]).unwrap();
            let q = ps5.seal(&hello).unwrap();
            let resp: Value = serde_json::from_slice(&ps5.open(&r.on_data(ch, &q[0]).unwrap()[0]).unwrap().unwrap()).unwrap();
            assert_eq!(resp["err"], "account", "{resp}");
            assert!(resp.get("owner").is_none(), "묶인 계정을 알려 주지 않는다: {resp}");
            assert!(!r.chans.contains_key(&ch), "통로를 닫는다");
        }
        assert_eq!(device(&conn, &hex(&pid)).unwrap().acct.as_deref(), Some("0123456789abcdef"), "묶인 계정은 바뀌지 않는다");

        // 다른 폰 키로 같은 pid·psk → 거절
        let other = generate_keypair().unwrap();
        r.chans.insert(4, Chan::Await { since: Instant::now() });
        let mut ini = TestInitiator::new(&other.private, &id.public, &psk, MODE_CONNECT, &pid, None);
        assert!(r.on_data(4, &first(MODE_CONNECT, &pid, &ini.first())).is_err());

        // 연결 해제 → 통로 닫힘, 기기 사라짐
        let q = ps3.seal(br#"{"id":9,"m":"unpair","p":{}}"#).unwrap();
        r.on_data(3, &q[0]).unwrap();
        assert!(device(&conn, &hex(&pid)).is_none());
        assert!(!r.chans.contains_key(&3));
    }

    #[test]
    fn phone_leaving_clears_pending_and_await_times_out() {
        let (shared, conn, id) = setup();
        let hooks = Hooks { on_pair_request: Box::new(|_, _| {}), on_local_change: Box::new(|_| {}) };
        let mut r = Runner { shared: &shared, conn: &conn, hooks: &hooks, host: "dev.conoti.app", env: "dev".into(), name: "PC", version: "t", id: id.clone(), chans: HashMap::new(), closing: Vec::new(), sent: VecDeque::new() };
        let uri = super::super::new_offer(&shared, "dev", &id);
        let s: [u8; 32] = unb64(uri.split("&s=").nth(1).unwrap()).unwrap().try_into().unwrap();
        let phone = generate_keypair().unwrap();
        r.chans.insert(1, Chan::Await { since: Instant::now() });
        let mut ini = TestInitiator::new(&phone.private, &id.public, &s, MODE_PAIR, &[0u8; 16], None);
        let m2 = r.on_data(1, &first(MODE_PAIR, &[0u8; 16], &ini.first())).unwrap();
        let (mut ps, h) = ini.finish_with_hash(&m2[0]).unwrap();
        r.on_data(1, &ps.seal(br#"{"id":1,"m":"hello","p":{"name":"x"}}"#).unwrap()[0]).unwrap();
        // PC 창과 폰이 같은 확인 코드를 본다
        assert_eq!(shared.pending.lock().unwrap().as_ref().unwrap().sas, crypto::sas(&h));
        // 폰이 나감 → 다음 틱에 요청이 치워진다(허용을 눌러도 아무 기기도 안 생긴다)
        r.chans.remove(&1);
        r.pairing_tick();
        assert!(shared.pending.lock().unwrap().is_none());
        assert!(super::super::devices(&conn).is_empty());
        // 핸드셰이크 없이 붙어 있는 통로는 10초 뒤 닫힌다
        r.chans.insert(7, Chan::Await { since: Instant::now() - Duration::from_secs(11) });
        let out = r.pairing_tick();
        assert!(out.iter().any(|(ch, _, close)| *ch == 7 && *close));
    }

    #[test]
    fn denied_pairing_stores_nothing() {
        let (shared, conn, id) = setup();
        let hooks = Hooks { on_pair_request: Box::new(|_, _| {}), on_local_change: Box::new(|_| {}) };
        let mut r = Runner { shared: &shared, conn: &conn, hooks: &hooks, host: "dev.conoti.app", env: "dev".into(), name: "PC", version: "t", id: id.clone(), chans: HashMap::new(), closing: Vec::new(), sent: VecDeque::new() };
        let uri = super::super::new_offer(&shared, "dev", &id);
        let s: [u8; 32] = unb64(uri.split("&s=").nth(1).unwrap()).unwrap().try_into().unwrap();
        let phone = generate_keypair().unwrap();
        r.chans.insert(1, Chan::Await { since: Instant::now() });
        let mut ini = TestInitiator::new(&phone.private, &id.public, &s, MODE_PAIR, &[0u8; 16], None);
        let m2 = r.on_data(1, &first(MODE_PAIR, &[0u8; 16], &ini.first())).unwrap();
        let mut ps = ini.finish(&m2[0]).unwrap();
        r.on_data(1, &ps.seal(br#"{"id":1,"m":"hello","p":{"name":"x"}}"#).unwrap()[0]).unwrap();
        shared.pending.lock().unwrap().as_mut().unwrap().decision = Some((false, true));
        let out = r.pairing_tick();
        assert!(out[0].2, "거절하면 통로를 닫는다");
        let resp: Value = serde_json::from_slice(&ps.open(&out[0].1[0]).unwrap().unwrap()).unwrap();
        assert_eq!(resp["err"], "denied");
        assert!(super::super::devices(&conn).is_empty());
    }

    #[test]
    fn wrong_pairing_secret_never_reaches_approval() {
        let (shared, conn, id) = setup();
        let asked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let a2 = asked.clone();
        let hooks = Hooks { on_pair_request: Box::new(move |_, _| a2.store(true, Ordering::SeqCst)), on_local_change: Box::new(|_| {}) };
        let mut r = Runner { shared: &shared, conn: &conn, hooks: &hooks, host: "dev.conoti.app", env: "dev".into(), name: "PC", version: "t", id: id.clone(), chans: HashMap::new(), closing: Vec::new(), sent: VecDeque::new() };
        super::super::new_offer(&shared, "dev", &id);
        let phone = generate_keypair().unwrap();
        r.chans.insert(1, Chan::Await { since: Instant::now() });
        let mut ini = TestInitiator::new(&phone.private, &id.public, &[1u8; 32], MODE_PAIR, &[0u8; 16], None);
        let m2 = r.on_data(1, &first(MODE_PAIR, &[0u8; 16], &ini.first())).unwrap();
        assert!(ini.finish(&m2[0]).is_err(), "폰이 PC 두 번째 메시지에서 psk 불일치를 안다");
        // 공격자가 그래도 무언가를 보내면 PC 는 복호화에 실패해 통로를 닫는다
        assert!(r.on_data(1, &[0u8; 40]).is_err());
        assert!(!asked.load(Ordering::SeqCst));
        assert!(!shared.offer.lock().unwrap().as_ref().unwrap().used);
    }

    /// 이미지 첨부 규격(docs/RELAY.md §4-1)을 dev 중계로: 올리기 → 답에 붙이기 → 받기 → 말풍선 atts.
    ///   cargo test live_dev_relay_images -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_dev_relay_images() {
        use base64::Engine;
        let b64s = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        let dir = std::env::temp_dir().join(format!("aiinbox-relay-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        crate::paths::set_data_dir_override(dir.clone());
        let path = dir.join("t.db");
        let c = db::open(&path).unwrap();
        db::migrate(&c).unwrap();
        db::set_meta(&c, "relay.enabled", "1").unwrap();
        db::set_meta(&c, "relay.env", "dev").unwrap();
        // 전달은 하지 않는다 — 확인 대기로 받아 두기만
        db::set_meta(&c, "conoti.confirm", "1").unwrap();
        db::set_meta(&c, "conoti.bg_resume", "1").unwrap();
        c.execute("INSERT INTO session (id, project_dir, first_at, last_at, live_name) VALUES ('sess-img', '/tmp', '2026-09-24T00:00:00Z', '2026-09-24T00:00:00Z', 'img-test')", []).unwrap();
        c.execute("INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, prompt_text, response_text, status) VALUES ('sess-img', 'u1', 1, '2026-09-24T00:00:00Z', '안녕', '반가워요', 'done')", []).unwrap();
        let kp = generate_keypair().unwrap();
        let id = Identity { key: kp.private, public: kp.public, secret: random(), psks: HashMap::new() };
        let shared = Shared::new();
        shared.set_identity_for_test(id.clone());
        let s2 = shared.clone();
        let hooks = Hooks {
            on_pair_request: Box::new(move |_, _| {
                if let Some(p) = s2.pending.lock().unwrap().as_mut() {
                    p.decision = Some((true, true));
                }
            }),
            on_local_change: Box::new(|_| {}),
        };
        spawn(shared.clone(), path.clone(), hooks, "test".into());
        let t0 = Instant::now();
        while shared.status.lock().unwrap().state != "online" {
            assert!(t0.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(100));
        }
        let uri = super::super::new_offer(&shared, "dev", &id);
        let s: [u8; 32] = unb64(uri.split("&s=").nth(1).unwrap()).unwrap().try_into().unwrap();
        let (mut ws, _) = tungstenite::connect("wss://dev.conoti.app/v1/relay/ws").unwrap();
        let sent_bytes = std::cell::Cell::new(0usize);
        let send = |ws: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>, v: Value| {
            let t = v.to_string();
            sent_bytes.set(sent_bytes.get() + t.len());
            ws.send(Message::Text(t)).unwrap()
        };
        let recv = |ws: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>| -> Value {
            loop {
                match ws.read().unwrap() {
                    Message::Text(t) => return serde_json::from_str(&t).unwrap(),
                    Message::Close(f) => panic!("서버가 닫음: {f:?}"),
                    _ => {}
                }
            }
        };
        send(&mut ws, json!({"t": "join", "v": 1, "room": id.room()}));
        assert_eq!(recv(&mut ws)["t"], "ready");
        let phone = generate_keypair().unwrap();
        let mut ini = TestInitiator::new(&phone.private, &id.public, &s, MODE_PAIR, &[0u8; 16], None);
        send(&mut ws, json!({"t": "d", "d": b64(&first(MODE_PAIR, &[0u8; 16], &ini.first()))}));
        let mut ps = ini.finish(&unb64(recv(&mut ws)["d"].as_str().unwrap()).unwrap()).unwrap();
        let mut n = 0;
        let mut call = |ws: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>, ps: &mut Session, m: &str, p: Value| -> Value {
            n += 1;
            for f in ps.seal(json!({"id": n, "m": m, "p": p}).to_string().as_bytes()).unwrap() {
                send(ws, json!({"t": "d", "d": b64(&f)}));
            }
            loop {
                let v = recv(ws);
                if v["t"] != "d" {
                    continue;
                }
                if let Some(m) = ps.open(&unb64(v["d"].as_str().unwrap()).unwrap()).unwrap() {
                    let m: Value = serde_json::from_slice(&m).unwrap();
                    if m.get("ev").is_none() {
                        return m;
                    }
                }
            }
        };
        assert_eq!(call(&mut ws, &mut ps, "hello", json!({"name": "시험 폰"}))["ok"], true);

        // 사진 셋: 작은 PNG 둘 + 1.2MB 남짓한 잡음 JPEG(조각 여럿으로 나뉜다)
        let png = |w: u32, rgb: [u8; 3]| {
            let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, w / 2, image::Rgb(rgb)));
            let mut b = std::io::Cursor::new(Vec::new());
            img.write_to(&mut b, image::ImageFormat::Png).unwrap();
            b.into_inner()
        };
        let noise = {
            let mut x: u32 = 12345;
            let img = image::RgbImage::from_fn(1100, 800, |_, _| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                image::Rgb([x as u8, (x >> 8) as u8, (x >> 16) as u8])
            });
            let mut b = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut b, 75).encode_image(&img).unwrap();
            b
        };
        println!("noise jpeg {} bytes", noise.len());
        assert!(noise.len() < 1_572_864 && noise.len() > 900_000);
        let rid = format!("img-{}-0001", std::process::id());
        let mut ids = vec![];
        for (i, bytes) in [png(64, [200, 0, 0]), png(96, [0, 0, 200]), noise.clone()].iter().enumerate() {
            let t = Instant::now();
            let r = call(&mut ws, &mut ps, "att", json!({"rid": rid, "i": i, "data": b64s(bytes)}));
            println!("att {i} → {} ({} ms)", r, t.elapsed().as_millis());
            assert_eq!(r["ok"], true, "{r}");
            ids.push(r["r"]["id"].as_str().unwrap().to_string());
        }
        // 재전송은 같은 결과
        let again = call(&mut ws, &mut ps, "att", json!({"rid": rid, "i": 0, "data": b64s(&png(64, [200, 0, 0]))}));
        assert_eq!(again["r"]["id"], ids[0].as_str());
        // 한도·형식
        // (1.5MB 넘는 한 장은 단위 시험에서 본다 — 여기서 보내면 폰 전송량 60초 4MiB 를 넘겨 서버가 끊는다: 폰 앱은 스스로 나눠 보낸다)
        assert_eq!(call(&mut ws, &mut ps, "att", json!({"rid": rid, "i": 3, "data": b64s(b"<svg/>")}))["err"], "bad_request");
        // 아직 답에 안 붙은 이미지는 못 받는다
        assert_eq!(call(&mut ws, &mut ps, "att_get", json!({"id": ids[0], "size": "thumb"}))["err"], "not_found");
        // 다른 답 번호로 올린 id 는 못 붙인다
        let r = call(&mut ws, &mut ps, "reply", json!({"sid": "sess-img", "text": "", "rid": "other-rid-0001", "atts": [ids[0]]}));
        assert_eq!(r["err"], "bad_request", "{r}");
        // 이미지만 있는 답
        let r = call(&mut ws, &mut ps, "reply", json!({"sid": "sess-img", "rid": rid, "atts": ids}));
        println!("reply → {}", r["r"]);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["r"]["state"], "confirm");
        assert_eq!(r["r"]["atts"].as_array().unwrap().len(), 3);
        // 답에 쓴 rid 로는 더 못 올린다
        assert_eq!(call(&mut ws, &mut ps, "att", json!({"rid": rid, "i": 4, "data": b64s(&png(32, [1, 2, 3]))}))["err"], "bad_request");
        // 받기
        for (size, i) in [("thumb", 2), ("view", 2), ("view", 0)] {
            let r = call(&mut ws, &mut ps, "att_get", json!({"id": ids[i], "size": size}));
            assert_eq!(r["ok"], true, "{r}");
            let data = base64::engine::general_purpose::STANDARD.decode(r["r"]["data"].as_str().unwrap()).unwrap();
            println!("att_get {size} #{i} → {} {}x{} {} bytes", r["r"]["mime"], r["r"]["w"], r["r"]["h"], data.len());
            if size == "thumb" {
                assert_eq!(r["r"]["mime"], "image/jpeg");
                assert!(r["r"]["w"].as_i64().unwrap() <= 320);
            }
        }
        // 전달된 요청이라고 치고 — 말풍선에 atts
        let block = crate::attach::block(&c, &ids).unwrap();
        c.execute(
            "INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, prompt_text, status) VALUES ('sess-img', 'u2', 2, '2026-09-24T01:00:00Z', ?1, 'done')",
            rusqlite::params![format!("{}\n요청: 안녕\n\n\n\n{block}", crate::conoti::REPLY_HEADER)],
        )
        .unwrap();
        let ch = call(&mut ws, &mut ps, "chat", json!({"sid": "sess-img", "limit": 5}));
        let last = ch["r"]["turns"].as_array().unwrap().last().unwrap().clone();
        println!("bubble prompt={:?} atts={}", last["prompt"], last["atts"]);
        assert_eq!(last["atts"].as_array().unwrap().len(), 3);
        assert!(!last["prompt"].as_str().unwrap_or("").contains("attachments"));
        println!("폰이 보낸 프레임 합계 {} bytes", sent_bytes.get());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// dev 중계(dev.conoti.app)를 실제로 거치는 왕복: PC 스레드 ↔ 서버 ↔ 시험용 폰.
    ///   cargo test live_dev_relay -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_dev_relay() {
        let dir = std::env::temp_dir().join(format!("aiinbox-relay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let c = db::open(&path).unwrap();
        db::migrate(&c).unwrap();
        db::set_meta(&c, "relay.enabled", "1").unwrap();
        db::set_meta(&c, "relay.env", "dev").unwrap();
        c.execute("INSERT INTO session (id, project_dir, first_at, last_at, live_name) VALUES ('sess-live', '/tmp', '2026-09-24T00:00:00Z', '2026-09-24T00:00:00Z', 'live-test')", []).unwrap();
        c.execute("INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, prompt_text, response_text, status) VALUES ('sess-live', 'u1', 1, '2026-09-24T00:00:00Z', '안녕', '반가워요', 'done')", []).unwrap();
        let kp = generate_keypair().unwrap();
        let id = Identity { key: kp.private, public: kp.public, secret: random(), psks: HashMap::new() };
        let shared = Shared::new();
        shared.set_identity_for_test(id.clone());
        let s2 = shared.clone();
        let hooks = Hooks {
            on_pair_request: Box::new(move |_, _| {
                if let Some(p) = s2.pending.lock().unwrap().as_mut() {
                    p.decision = Some((true, true));
                }
            }),
            on_local_change: Box::new(|_| {}),
        };
        spawn(shared.clone(), path.clone(), hooks, "test".into());
        let t0 = Instant::now();
        while shared.status.lock().unwrap().state != "online" {
            assert!(t0.elapsed() < Duration::from_secs(20), "PC 가 중계에 붙지 못함: {:?}", shared.status.lock().unwrap().error);
            std::thread::sleep(Duration::from_millis(100));
        }
        let uri = super::super::new_offer(&shared, "dev", &id);
        let s: [u8; 32] = unb64(uri.split("&s=").nth(1).unwrap()).unwrap().try_into().unwrap();

        // 폰 역할
        let (mut ws, _) = tungstenite::connect("wss://dev.conoti.app/v1/relay/ws").unwrap();
        let send = |ws: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>, v: Value| ws.send(Message::Text(v.to_string())).unwrap();
        let recv = |ws: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>| -> Value {
            loop {
                if let Message::Text(t) = ws.read().unwrap() {
                    return serde_json::from_str(&t).unwrap();
                }
            }
        };
        send(&mut ws, json!({"t": "join", "v": 1, "room": id.room()}));
        assert_eq!(recv(&mut ws)["t"], "ready");
        let phone = generate_keypair().unwrap();
        let mut ini = TestInitiator::new(&phone.private, &id.public, &s, MODE_PAIR, &[0u8; 16], None);
        send(&mut ws, json!({"t": "d", "d": b64(&first(MODE_PAIR, &[0u8; 16], &ini.first()))}));
        let m2 = unb64(recv(&mut ws)["d"].as_str().unwrap()).unwrap();
        let mut ps = ini.finish(&m2).unwrap();
        let rpc_call = |ws: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>, ps: &mut Session, req: Value| -> Value {
            for f in ps.seal(req.to_string().as_bytes()).unwrap() {
                send(ws, json!({"t": "d", "d": b64(&f)}));
            }
            loop {
                let v = recv(ws);
                if v["t"] != "d" {
                    continue;
                }
                if let Some(m) = ps.open(&unb64(v["d"].as_str().unwrap()).unwrap()).unwrap() {
                    let m: Value = serde_json::from_slice(&m).unwrap();
                    if m.get("ev").is_none() {
                        return m;
                    }
                }
            }
        };
        let hello = rpc_call(&mut ws, &mut ps, json!({"id": 1, "m": "hello", "p": {"name": "시험 폰"}}));
        assert_eq!(hello["ok"], true, "{hello}");
        assert!(hello["r"]["paired"]["psk"].is_string());
        let ss = rpc_call(&mut ws, &mut ps, json!({"id": 2, "m": "sessions", "p": {}}));
        assert_eq!(ss["r"]["items"][0]["name"], "live-test");
        let tid = rpc_call(&mut ws, &mut ps, json!({"id": 3, "m": "chat", "p": {"sid": "sess-live"}}))["r"]["turns"][0]["id"].as_i64().unwrap();
        let doc = rpc_call(&mut ws, &mut ps, json!({"id": 4, "m": "turn", "p": {"id": tid}}));
        assert!(doc["r"]["markdown"].as_str().unwrap().contains("반가워요"));
        // 세션 관리(규격 §4-2) — 보관 → 보관함 → 되돌리기 → 지우기
        assert_eq!(hello["r"]["can_manage"], true);
        let sid = json!(["sess-live"]);
        let r = rpc_call(&mut ws, &mut ps, json!({"id": 5, "m": "manage", "p": {"op": "archive", "sids": sid}}));
        assert_eq!(r["r"]["n"], 1, "{r}");
        let ss = rpc_call(&mut ws, &mut ps, json!({"id": 6, "m": "sessions", "p": {}}));
        assert!(ss["r"]["items"].as_array().unwrap().is_empty());
        let ar = rpc_call(&mut ws, &mut ps, json!({"id": 7, "m": "sessions", "p": {"filter": "archived"}}));
        assert_eq!(ar["r"]["items"][0]["archived"], true, "{ar}");
        let r = rpc_call(&mut ws, &mut ps, json!({"id": 8, "m": "manage", "p": {"op": "unarchive", "sids": sid}}));
        assert_eq!(r["r"]["n"], 1);
        let r = rpc_call(&mut ws, &mut ps, json!({"id": 9, "m": "tidy", "p": {"kind": "short"}}));
        assert_eq!(r["ok"], true, "{r}");
        let r = rpc_call(&mut ws, &mut ps, json!({"id": 10, "m": "manage", "p": {"op": "delete", "sids": sid}}));
        assert_eq!(r["r"]["deleted"], json!(["sess-live"]), "{r}");
        let gone = rpc_call(&mut ws, &mut ps, json!({"id": 11, "m": "chat", "p": {"sid": "sess-live"}}));
        assert_eq!(gone["err"], "not_found");
        println!("dev 중계 왕복 OK: {} · 세션 관리 OK", doc["r"]["title"]);
        let _ = ws.close(None);
        db::set_meta(&c, "relay.enabled", "0").unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 시험용 DB 경로 — 실제 데이터 폴더의 DB 는 거부하고, 사진 등 데이터 폴더도 DB 옆으로 돌린다(실제 폴더에 쓰지 않게).
    fn sim_db() -> std::path::PathBuf {
        let path = std::path::PathBuf::from(std::env::var("AI_INBOX_SIM_DB").expect("AI_INBOX_SIM_DB"));
        let real = crate::paths::data_dir();
        let abs = path.canonicalize().unwrap_or_else(|_| path.clone());
        assert!(
            !abs.starts_with(real.canonicalize().unwrap_or(real)),
            "실제 데이터 폴더의 DB 는 쓰지 않는다 — 복사본을 다른 폴더에 두고 넘겨라"
        );
        crate::paths::set_data_dir_override(abs.parent().expect("DB 폴더").to_path_buf());
        path
    }

    /// 시뮬레이터 E2E 용 PC 역할: 실제 DB 의 **복사본**(또는 가짜 기록으로 만든 DB)으로 dev 중계에 붙고, 페어링을 자동 허용한다(시험 코드에만 있음).
    ///   AI_INBOX_SIM_DB=<복사본 경로> AI_INBOX_SIM_OUT=<페어링 주소를 쓸 파일> AI_INBOX_SIM_SECS=900 \
    ///   cargo test serve_dev_for_simulator -- --ignored --nocapture
    #[test]
    #[ignore]
    fn serve_dev_for_simulator() {
        let path = sim_db();
        let out = std::env::var("AI_INBOX_SIM_OUT").expect("AI_INBOX_SIM_OUT");
        let secs: u64 = std::env::var("AI_INBOX_SIM_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(900);
        let c = db::open(&path).unwrap();
        db::migrate(&c).unwrap();
        db::set_meta(&c, "relay.enabled", "1").unwrap();
        db::set_meta(&c, "relay.env", "dev").unwrap();
        // 폰 입력창까지 시험할 때는 AI_INBOX_SIM_BG_RESUME=1 — 꺼진 세션이 답을 받는 것처럼 보인다(전달 스레드가 없어 실제로 넣지는 않는다)
        let bg = if std::env::var("AI_INBOX_SIM_BG_RESUME").as_deref() == Ok("1") { "1" } else { "0" };
        db::set_meta(&c, "conoti.bg_resume", bg).unwrap();
        let kp = generate_keypair().unwrap();
        let id = Identity { key: kp.private, public: kp.public, secret: random(), psks: HashMap::new() };
        let shared = Shared::new();
        shared.set_identity_for_test(id.clone());
        let s2 = shared.clone();
        let hooks = Hooks {
            on_pair_request: Box::new(move |name, sas| {
                println!("페어링 요청: {name} · 확인 코드 {sas} → 4초 뒤 자동 허용(폰 대기 화면을 찍을 틈)");
                let s3 = s2.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(4));
                    if let Some(p) = s3.pending.lock().unwrap().as_mut() {
                        p.decision = Some((true, true));
                    }
                });
            }),
            on_local_change: Box::new(|s| println!("폰이 바꿈: {s:?}")),
        };
        spawn(shared.clone(), path, hooks, "sim".into());
        let t0 = Instant::now();
        while shared.status.lock().unwrap().state != "online" {
            assert!(t0.elapsed() < Duration::from_secs(20), "중계에 붙지 못함");
            std::thread::sleep(Duration::from_millis(100));
        }
        let uri = super::super::new_offer(&shared, "dev", &id);
        std::fs::write(&out, &uri).unwrap();
        println!("페어링 주소를 {out} 에 씀. {secs}초 동안 대기");
        let mut last = String::new();
        while t0.elapsed() < Duration::from_secs(secs) {
            let st = shared.status.lock().unwrap().clone();
            let line = format!("{} 폰 {}대", st.state, st.phones_online);
            if line != last {
                println!("{line}");
                last = line;
            }
            // 제안이 쓰였거나 만료되면 새로 만든다(여러 번 시험할 수 있게)
            let renew = shared.offer.lock().unwrap().as_ref().map(|o| o.used || Instant::now() > o.expires).unwrap_or(true);
            if renew {
                std::fs::write(&out, super::super::new_offer(&shared, "dev", &id)).unwrap();
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// 폰 답 → dev 중계 → PC → 채널 → 실제 Claude 세션까지. 채널과 함께 떠 있는 세션이 있어야 한다.
    ///   AI_INBOX_SIM_DB=<실제 DB 복사본> AI_INBOX_REPLY_SID=<세션 id> cargo test live_reply_to_channel -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_reply_to_channel() {
        let path = sim_db();
        let sid = std::env::var("AI_INBOX_REPLY_SID").expect("AI_INBOX_REPLY_SID");
        let c = db::open(&path).unwrap();
        db::migrate(&c).unwrap();
        db::set_meta(&c, "relay.enabled", "1").unwrap();
        db::set_meta(&c, "relay.env", "dev").unwrap();
        let kp = generate_keypair().unwrap();
        let id = Identity { key: kp.private, public: kp.public, secret: random(), psks: HashMap::new() };
        let shared = Shared::new();
        shared.set_identity_for_test(id.clone());
        let s2 = shared.clone();
        let hooks = Hooks {
            on_pair_request: Box::new(move |_, _| {
                if let Some(p) = s2.pending.lock().unwrap().as_mut() {
                    p.decision = Some((true, true));
                }
            }),
            on_local_change: Box::new(|_| {}),
        };
        spawn(shared.clone(), path.clone(), hooks, "test".into());
        let p2 = path.clone();
        std::thread::spawn(move || {
            let mut pipe = crate::conoti::Pipeline::new(db::open(&p2).unwrap());
            loop {
                pipe.tick(&crate::deliver::LocalDeliver { bg_resume: false });
                std::thread::sleep(Duration::from_millis(500));
            }
        });
        let t0 = Instant::now();
        while shared.status.lock().unwrap().state != "online" {
            assert!(t0.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(100));
        }
        let uri = super::super::new_offer(&shared, "dev", &id);
        let s: [u8; 32] = unb64(uri.split("&s=").nth(1).unwrap()).unwrap().try_into().unwrap();
        let (mut ws, _) = tungstenite::connect("wss://dev.conoti.app/v1/relay/ws").unwrap();
        let send = |ws: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>, v: Value| ws.send(Message::Text(v.to_string())).unwrap();
        let recv = |ws: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>| -> Value {
            loop {
                if let Message::Text(t) = ws.read().unwrap() {
                    return serde_json::from_str(&t).unwrap();
                }
            }
        };
        send(&mut ws, json!({"t": "join", "v": 1, "room": id.room()}));
        assert_eq!(recv(&mut ws)["t"], "ready");
        let phone = generate_keypair().unwrap();
        let mut ini = TestInitiator::new(&phone.private, &id.public, &s, MODE_PAIR, &[0u8; 16], None);
        send(&mut ws, json!({"t": "d", "d": b64(&first(MODE_PAIR, &[0u8; 16], &ini.first()))}));
        let mut ps = ini.finish(&unb64(recv(&mut ws)["d"].as_str().unwrap()).unwrap()).unwrap();
        let mut n = 0;
        let mut call = |ws: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>, ps: &mut Session, m: &str, p: Value| -> Value {
            n += 1;
            for f in ps.seal(json!({"id": n, "m": m, "p": p}).to_string().as_bytes()).unwrap() {
                send(ws, json!({"t": "d", "d": b64(&f)}));
            }
            loop {
                let v = recv(ws);
                if v["t"] != "d" {
                    continue;
                }
                if let Some(m) = ps.open(&unb64(v["d"].as_str().unwrap()).unwrap()).unwrap() {
                    let m: Value = serde_json::from_slice(&m).unwrap();
                    if m.get("ev").is_none() {
                        return m;
                    }
                }
            }
        };
        assert_eq!(call(&mut ws, &mut ps, "hello", json!({"name": "시험 폰"}))["ok"], true);
        let head = call(&mut ws, &mut ps, "chat", json!({"sid": sid, "limit": 3}))["r"]["session"].clone();
        println!("세션 머리: can_reply={} reply_block={:?} channel={}", head["can_reply"], head["reply_block"], head["channel"]);
        assert_eq!(head["can_reply"], true, "채널이 살아 있어야 한다");
        let rid = format!("e2e-{}", std::process::id());
        let r = call(&mut ws, &mut ps, "reply", json!({"sid": sid, "text": "E2E 시험입니다. '폰 답 받았음' 이라고만 답해 주세요.", "rid": rid}));
        println!("reply → {}", r["r"]);
        let t1 = Instant::now();
        loop {
            let ch = call(&mut ws, &mut ps, "chat", json!({"sid": sid, "limit": 1}));
            let st = ch["r"]["replies"].as_array().and_then(|a| a.iter().find(|x| x["rid"] == rid.as_str())).map(|x| (x["state"].clone(), x["note"].clone()));
            if let Some((state, note)) = &st {
                if state != "delivering" {
                    println!("상태: {state} ({note})");
                    assert_eq!(state, "delivered");
                    break;
                }
            }
            assert!(t1.elapsed() < Duration::from_secs(30), "전달되지 않음: {st:?}");
            std::thread::sleep(Duration::from_millis(700));
        }
        let _ = ws.close(None);
        db::set_meta(&c, "relay.enabled", "0").unwrap();
        std::thread::sleep(Duration::from_millis(1500));
    }
}
