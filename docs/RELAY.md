# 폰 연결 규격 — 종단간 암호화 중계 (v1)

PC 의 AI Inbox 와 폰의 코노티 앱이 **코노티 서버를 중계로만** 써서 대화한다.
서버는 두 연결을 짝지어 암호문을 흘려보낼 뿐 **내용을 모르고, 디스크에 아무것도 쓰지 않는다.**
원본은 PC 의 SQLite 하나다. 폰은 볼 때마다 PC 에 묻는다.

```
 [PC: AI Inbox]  ── 암호문 ──▶  [코노티 중계]  ── 암호문 ──▶  [폰: 코노티 앱]
  SQLite 원본                   짝짓기는 메모리만              화면에서만 복호화
```

| 서버가 보는 것 | 서버가 못 보는 것 |
|---|---|
| 접속 IP · 접속 시각 · 메시지 크기와 횟수 · 푸시 횟수 · 앱 버전 · 폰의 기기 번호(`pid` — 연결 모드 첫 메시지에 평문으로 실린다. 같은 폰이 다시 붙은 것은 알아볼 수 있다) | 프롬프트 · 응답 · 요약 · 세션 이름 · 프로젝트 경로 · 폰 답 |

푸시를 켜 두면 서버는 "이 PC(IP)가 이 코노티 계정에 알림을 보낸다"는 것까지는 안다(티켓을 풀면 계정이 나온다). 내용은 여전히 모른다.

이 문서가 정본이다. PC 구현은 `src-tauri/src/relay/`, 서버·앱 구현은 코노티 레포(`features/relay`)에 있다.

---

## 1. 중계 서버 (평문 JSON 텍스트 프레임)

주소: `wss://<호스트>/v1/relay/ws` — 호스트는 `prod` = `conoti.app`, `dev` = `dev.conoti.app`.

연결 후 10초 안에 첫 프레임을 보내야 한다.

| 방향 | 프레임 | 뜻 |
|---|---|---|
| PC → 서버 | `{"t":"host","v":1,"secret":"<b64url 32B>"}` | 방 열기. 방 번호 = `b64url(SHA-256("conoti-relay-room/1" ‖ secret)[0..16])` |
| 서버 → PC | `{"t":"ready","room":"<방 번호>"}` | |
| 폰 → 서버 | `{"t":"join","v":1,"room":"<방 번호>"}` | 방 들어가기 |
| 서버 → 폰 | `{"t":"ready"}` / `{"t":"bye","code":"offline"}` | PC 가 없으면 즉시 닫는다 |
| 서버 → PC | `{"t":"open","ch":<n>}` | 폰 하나가 들어옴 (`ch` = 서버가 붙인 통로 번호) |
| 폰 → 서버 → PC | `{"t":"d","d":"<b64url>"}` → `{"t":"d","ch":n,"d":…}` | 암호문 |
| PC → 서버 → 폰 | `{"t":"d","ch":n,"d":…}` → `{"t":"d","d":…}` | 암호문 |
| PC → 서버 | `{"t":"close","ch":n}` | 그 폰을 끊는다 (폰은 `bye closed`) |
| 서버 → PC | `{"t":"close","ch":n}` | 폰이 나감 |
| 서버 → 폰 | `{"t":"bye","code":"offline"}` | PC 가 나감 |
| 양쪽 | `{"t":"ping"}` → `{"t":"pong"}` | 30초마다. 90초 동안 아무 프레임이 없으면 서버가 닫는다 |
| 서버 → 양쪽 | `{"t":"bye","code":"<사유>"}` | `replaced`(같은 방에 PC 가 새로 붙음) · `limit` · `bad_frame` · `timeout` · `closed` |

- `secret` 은 PC 만 안다. 폰은 방 번호만 안다. 그래서 폰이 PC 행세를 해서 방을 가로챌 수 없다.
  같은 `secret` 으로 PC 가 다시 붙으면 앞 연결을 `replaced` 로 끊는다(재시작 대비).
- 제한(2026-09-24 보안 점검 뒤): 웹소켓 프레임 128KiB·압축 없음(서버가 끊는다, 1009) · JSON 프레임 100,000자 ·
  연결당 60초에 600프레임 · 60초 전송량 PC 48MiB·폰 4MiB·IP 합계 64MiB · 방당 폰 4대(한 IP 는 2대까지) ·
  IP 당 동시 연결 8·방 3 · 서버 전체 연결 200·방 200 · 브라우저 Origin 은 코노티 웹 주소만. 넘으면 `bye: limit`.
  PC 는 "바뀜" 알림을 3초에 한 번까지만 보낸다(폰이 알림마다 다시 받기 때문).
  IP 는 앞단 프록시가 새로 쓰는 `X-Forwarded-For` 의 마지막 값으로 센다(`X-Real-IP` 는 클라이언트가 위조할 수 있어 쓰지 않는다).
- PC 는 핸드셰이크를 10초 안에 안 하는 통로, 핸드셰이크 뒤 30초 동안 말이 없는 통로를 닫는다(폰 자리 점거 방지).
- 서버는 `d` 를 해석하지 않고, 로그에 남기지 않는다.

## 2. 종단간 암호 — Noise `IKpsk2_25519_ChaChaPoly_SHA256`

- 먼저 말하는 쪽(initiator) = **폰**, 받는 쪽(responder) = **PC**.
  폰은 페어링 때 QR 로 PC 정적 공개키를 받아 두므로 IK 가 맞다.
- 폰의 첫 `d` 평문 = `mode(1B) ‖ pid(16B) ‖ Noise 첫 메시지`.
  - `mode 0x01` 페어링: `psk` = QR 의 `s`(32B), `pid` = 0 16바이트.
  - `mode 0x02` 연결: `psk` = 페어링 때 받은 `psk`(32B), `pid` = 페어링 번호.
- prologue = ASCII `conoti-ai-relay/1` ‖ `mode` ‖ `pid` — 방식·번호가 바뀌면 핸드셰이크가 깨진다.
- Noise 첫 메시지 페이로드는 비워 둔다. PC 는 두 번째 메시지(페이로드 비움)로 답한다.
  이후 모든 `d` 는 Noise 전송 메시지다.
- 연결 모드에서 PC 는 `pid` 로 저장된 폰 정적키를 찾아 핸드셰이크 뒤 **상대 정적키가 같은지** 확인한다. 다르면 끊는다.
- 복호화가 한 번이라도 실패하면 그 통로를 끊는다. 순서는 웹소켓이 보장한다.

### 2-1. 큰 메시지 나누기

Noise 메시지는 65,535바이트가 한계다. 앱 메시지(UTF-8 JSON)를 60,000바이트 조각으로 나누고,
각 조각 앞에 1바이트를 붙여 암호화한다: `0x00` = 뒤에 더 있음, `0x01` = 마지막.
받는 쪽은 `0x01` 이 올 때까지 이어 붙인다. 합친 메시지는 4 MiB 를 넘을 수 없다.

## 3. 페어링

1. PC 가 **페어링 제안**을 만든다: `s` = 무작위 32B, 5분 유효, 한 번만 쓴다.
2. PC 화면에 QR(과 복사용 문자열)을 띄운다:
   `conoti-ai://pair?v=1&e=<prod|dev>&r=<방 번호>&k=<PC 정적 공개키 b64url>&s=<b64url>`
3. 폰이 읽고, 자기 환경(`e`)과 다르면 거절한다. 폰은 자기 정적키(X25519)를 만들어 보안 저장소에 둔다.
4. 폰이 `mode 0x01` 로 들어와 핸드셰이크를 마치고 `hello` 를 보낸다(§4).
5. PC 는 **사용자에게 허용을 묻는다**(기기 이름 · **확인 코드** · "이 기기에서 세션에 답 보내기 허용" 체크, 2분 제한).
   확인 코드 = `u32_be(SHA-256("conoti-ai-sas/1" ‖ h)[0..4]) % 1_000_000` 을 6자리로(`123 456`). `h` 는 Noise 핸드셰이크 해시.
   폰도 같은 코드를 보여 주고, 사용자는 두 화면의 숫자가 같을 때만 허용한다 — QR 문자열이 새어 다른 기기가 먼저 끼어들어도 여기서 걸린다.
   허용 대기 중 그 폰이 나가면 요청도 치운다.
6. 허용하면 PC 가 `pid`(16B) · `psk`(32B) 를 만들어 저장하고 `hello` 응답에 실어 보낸다.
   폰은 `{방 번호, PC 공개키, pid, psk, PC 이름}` 을 저장한다. 이 통로는 그대로 이어서 쓴다.
7. 거절·시간 초과면 PC 가 `denied` 오류로 답하고 통로를 닫는다.

QR 은 카메라가 PC 화면에서 직접 읽으므로 서버가 키를 바꿔칠 수 없다.

## 4. 앱 메시지 (복호화된 JSON)

요청 `{"id":<정수>,"m":"<이름>","p":{…}}` → 응답 `{"id":…,"ok":true,"r":{…}}` 또는
`{"id":…,"ok":false,"err":"<코드>","msg":"<설명>"}`.
PC 가 먼저 보내는 알림: `{"ev":"changed"}`(무언가 바뀜 — 보고 있는 화면을 다시 불러라, 1초에 한 번까지).

| 이름 | 보내는 값 `p` | 받는 값 `r` |
|---|---|---|
| `hello` | `{name, app, ticket?, acct?}` | `{desktop, version, can_reply, can_manage, paired?:{pid, psk, room}}` — `paired` 는 페어링 모드에서만. `pid`(16B)·`psk`(32B)는 b64url. `can_manage` 는 0.5.0 부터(없으면 false) |
| `sessions` | `{filter?: "all"\|"unread"\|"attention"\|"active"\|"archived"}` | `{items:[Session], unread, attention, active, can_manage, archived?, tidy_short?, tidy_idle?}` — `archived`(보관함)·뒤의 세 수는 기록 관리를 허용한 기기만(§4-2) |
| `chat` | `{sid, before?: seq, limit?: 1..60}` | `{session: SessionHead, turns:[Bubble], has_more, replies:[Reply]}` |
| `turn` | `{id}` | `{id, sid, title, markdown, status, needs_input, prev_id, next_id}` |
| `read` | `{ids:[…]}` 또는 `{sid}` | `{}` |
| `reply` | `{sid, turn_id?, text, rid, atts?:[id…], quote?: "prompt"\|"response"}` | `Reply` — `quote` 는 메신저식 답장(아래) |
| `att` | `{rid, i, data}` | `{id, w, h, bytes}` — 이미지 한 장 올리기(§4-1) |
| `att_get` | `{id, size: "thumb"\|"view"}` | `{id, mime, w, h, data}` — 이미지 받기(§4-1) |
| `unpair` | `{}` | `{}` — PC 가 이 기기를 지우고 통로를 닫는다 |
| `manage` | `{op: "archive"\|"unarchive"\|"pin"\|"unpin"\|"delete", sids:[sid…]}` | `{n, deleted?:[sid…], skipped?:[이름…]}` — 세션 관리(§4-2) |
| `tidy` | `{kind: "short"\|"idle"}` | `{n}` — 정리 제안대로 한 번에 보관(§4-2) |

- `ticket` 은 §5 의 푸시 티켓. 폰은 연결할 때마다 새로 받아 넘긴다.
- `acct` 는 로그인 사용자 id 로 만든 `hex(SHA-256("conoti-ai-acct/1" ‖ user_id))[0..16]`. 같은 계정의 폰이 여럿이면
  PC 가 푸시를 계정당 한 번만 보내는 데 쓴다(서버에는 가지 않는다).
- **기기는 처음 연결한 코노티 계정(`acct`)에 묶인다**(0.5.1). 연결 모드의 통로는 `hello` 가 통과해야 다른 요청을 받는다(`hello_required`).
  `hello` 의 `acct` 가 묶인 계정과 다르거나 없으면 `{"err":"account"}` 로 거절하고 통로를 닫는다 —
  같은 폰에서 다른 계정으로 로그인하면 그 PC 를 볼 수 없다. 폰은 그 연결을 지운다(주인 계정으로 다시 로그인하면 새로 연결한다).
  계정 없이 페어링한 옛 기기는 처음 온 계정에 묶인다. (0.5.1~0.5.2 는 `owner` 로 묶인 계정을 알려 주었다 — 0.5.3 부터 싣지 않는다.)
  ⚠️ 이것은 **보안 경계가 아니다** — `acct` 는 폰이 스스로 알리는 값이다. 기기 키를 가진 폰(= 이미 허용한 폰)이 값을 꾸미는 것은 막지 못한다.
  목적은 정직한 앱에서 계정마다 연결을 나누는 것이고, 접근 통제는 기기 키 + PC 의 허용이 맡는다.
- 오류 코드: `bad_request` · `not_found` · `rejected` · `unknown_method` · `hello_required` · `account` · `unpaired`(PC 에서 해제됨 — 폰은 저장된 연결을 지운다) ·
  페어링 중 `denied`(거절·2분 초과) · `busy`(다른 기기가 허용 대기 중).
- `rid` 는 폰이 만든 UUID. 같은 `rid` 로 다시 보내면 처음 결과를 돌려준다.
- `text` 는 1~4,000자(이미지를 붙이면 0자도 된다). 제어문자는 줄바꿈·탭만 허용.

### 4-1. 이미지 첨부 (한 메시지에 5장까지)

폰은 사진을 **한 장씩 먼저 올리고**(`att`), 받은 id 들을 `reply` 의 `atts` 에 실어 보낸다.

- `att` `{rid, i, data}` — `rid` 는 이 이미지가 붙을 답의 `rid`, `i` 는 0~4 순서, `data` 는 이미지 바이트의 표준 base64(줄바꿈 없음).
  - 폰은 긴 변 1600px·JPEG(품질 80 안팎)로 줄여서 보낸다. PC 는 풀었을 때 **1.5 MiB(1,572,864바이트)를 넘으면** `bad_request`
    (한 장이 암호화·base64 두 번을 거치면 약 1.8배 = 2.7 MiB — 한 메시지 4 MiB·폰 전송량 60초 4 MiB 안에 들어야 한다). 넘으면 폰이 품질을 낮춰 다시 줄인다.
  - 형식은 PC 가 바이트 머리로 정한다 — JPEG·PNG·WebP·GIF 만. 가로·세로 1~12,000px, 1,600만 화소·8비트 색까지(풀기 전에 머리로 판정).
  - `can_reply` 가 꺼진 기기는 `rejected`. 같은 기기의 같은 `(rid, i)` 를 다시 보내면 처음 결과를 돌려준다.
  - 올리고 1시간 안에 `reply` 에 쓰지 않으면 PC 가 지운다(그 뒤 같은 `(rid, i)` 를 다시 올리면 새로 저장해 돌려준다 — 폰은 45분이 지나면 다시 올린다).
    한 기기가 아직 쓰지 않은 이미지는 10장까지, 하루 100장까지 — 넘으면 `rejected`. 거절된 답(세션 없음·막힘 등)에 붙인 이미지는 바로 지운다.
  - `id` = 저장한 바이트의 SHA-256 소문자 hex 64자. `bytes` = 저장한 크기.
- `reply.atts` — 이 기기가 **같은 `rid` 로 올린** id 만, 0~5개, 순서대로. 다른 기기·다른 답의 id 는 `bad_request`.
  응답 `Reply` 의 `atts` 에 붙은 id 를 되돌려 준다. 이미 답에 쓴 `rid` 로 `att` 를 더 올리면 `bad_request`.
- `att_get` `{id, size}` — 숨기지 않은 세션의 메시지에 붙은 이미지만(아니면 `not_found`).
  - `thumb` = 긴 변 320px JPEG, `view` = 긴 변 1600px JPEG(원본이 JPEG·PNG 이고 1600px·1.5MB 이하이면 원본 그대로 — `mime` 을 보라).
  - `data` 는 표준 base64.
- 전송량: 서버는 프레임 JSON 텍스트 전체(`{"t":"d","d":…}`)의 바이트를 센다. 폰은 서버 한도(60초 4 MiB)를 넘지 않게 **스스로 60초 3.5 MiB 까지만 보내고 나머지는 기다린다.**
  이미지 한 장(≈300 KB)은 암호화·base64 두 번을 거쳐 약 1.8배가 된다.
- PC 는 이미지를 앱 데이터 폴더 `attachments/` 에 id 이름으로 한 번만 저장하고(같은 이미지는 한 벌), 세션에는 경로 목록을 붙여 넣는다:

```
<본문>

[첨부 이미지 2장 — Read 도구로 열어 보세요]
/…/attachments/ab/ab12…(64자).jpg
/…/attachments/cd/cd34…(64자).png
```

### 4-2. 세션 관리 (0.5.0) — 데이터는 PC 에만, 폰은 요청만

- PC 가 기기마다 **"기록 관리 허용"** 을 둔다. 처음 값은 그 기기의 "답 보내기 허용"과 같다(페어링 때 고른 값 · 0.5.0 으로 올라올 때 이미
  연결된 기기도 같은 규칙 — 답을 막아 둔 기기에 지우기 권한이 생기지 않게). 꺼진 기기는 `sessions{filter:"archived"}`·`manage`·`tidy` 가 `rejected`,
  보관한 세션은 `chat`·`turn`·`att_get`·`read` 에서 `not_found`, `reply` 는 없는 세션과 같은 거절(예전처럼 폰에 없는 세션 — 보관 여부를 알 수 없다).
  켜진 기기만 보관함을 보고 보관한 세션을 연다. PC 의 "폰 답 받기 멈춤"이 켜져 있으면 `manage`·`tidy` 도 `rejected`.
- 보관한 세션은 폰에서 **읽기만** 된다 — `reply_block = "archived"`. 목록으로 되돌리면(`unarchive`) 답할 수 있다.
- `sids` 는 1~200개(중복 제거). `pin`·`archive` 등은 이미 그 상태면 세지 않는다(`n`).
- `delete` 는 PC 화면의 "기록에서 지우기"와 같다: **이 앱의 사본만** 지우고 Claude Code 원본은 건드리지 않는다.
  진행 중인 요청·일하는 프로세스·전달 대기 말이 있는 세션은 남기고 `skipped` 에 이름을 싣는다. 세션별 "폰 답 막기"는 남는다.
- `tidy` — `short`: 요청 1개 이하 · `idle`: 30일 넘게 조용함. 고정·실행 중·진행 중·안 읽은 결과·전달 대기 말이 있는 세션은 빠진다.
- 성공하면 PC 가 모든 폰에 `{"ev":"changed"}` 를 보낸다(PC 화면도 다시 읽는다). 보관한 세션도 새 요청·결과가 오면 저절로 목록으로 돌아온다.

### 객체 모양

```jsonc
// Session — 세션 목록 한 줄
{ "id":"…", "name":"api-refactor", "named":true, "project":"my-app", "dir":"~/code/my-app",
  "branch":"main", "live":"busy|idle|null", "pinned":false, "model":"claude-opus-5-5",
  "turns":12, "unread":2, "attention":0, "active":1,
  "last_status":"done", "last_needs_input":false, "preview":"…", "preview_ai":true, "last_at":"ISO8601",
  "archived":false, "agent":"claude|codex" }
  // agent(0.6.0~): 세션을 만든 도구. 없으면 claude. codex 세션의 model 은 gpt-… 이고 cost 는 없다

// SessionHead — 대화 머리
{ "id":"…", "name":"…", "named":true, "project":"…", "dir":"…", "branch":"…", "live":"busy",
  "model":"…", "cost_usd":1.23, "turns":12, "unread":2,
  "can_reply":true, "reply_block":"", "channel":true, "pinned":false, "archived":false, "agent":"claude|codex" }
  // reply_block: 빈 문자열 = 답 가능. 아니면 코드(폰이 번역): device_off · paused · session_blocked · no_session · archived(보관한 세션) ·
  //              no_channel(터미널에서 채널 없이 실행 중) · offline_no_resume(꺼진 세션·백그라운드 세션인데 이어서 실행 꺼짐)
  //              백그라운드 세션이 일하는 중이면 "" — 답은 PC 에서 기다렸다가 하던 일이 끝나면 들어간다
  //              codex 세션: 열려 있으면 "" (Codex 대기열로) · 꺼져 있으면 이어서 실행 설정에 따라 "" 또는 offline_no_resume · no_channel 은 없다
  //              (받은 뒤 3시간 상한 · 일하는 동안 기다린 시간은 빼고 10분 넘게 못 넣으면 넣지 않음)

// Bubble — 요청 한 쌍(내 말 + AI 결과)
{ "id":42, "seq":7, "origin":"human|peer|channel|…", "peer":null, "via_phone":false, "mid_turn":false,
  "prompt_at":"ISO", "prompt":"…(4,000자까지)", "slash":null,
  "status":"running|background|waiting|done|interrupted|stopped", "needs_input":false, "pending_bg":0,
  "summary":"…", "response":"…(6,000자까지)", "step":"지금 하는 일 한 줄|null", "atts":["<id>",…],
  "ended_at":"ISO|null", "duration_ms":192000, "tool_calls":14, "files":3, "agents":0,
  "out_tokens":5200, "model":"…", "unread":true, "starred":false,
  "quote":null }

// quote: 메신저식 답장(PC 0.7.0) — {"seq":12, "part":"prompt|response", "text":"발췌 한 줄(240자까지)"} · 없으면 null.
//   이때 prompt 는 답장 줄을 뗀 본문이다. 폰은 말풍선 위에 인용으로 그린다.
// mid_turn: 앞 요청이 진행되는 중에 보낸 말(Claude Code 의 queued_command) — 답은 그 직후 모델의 첫 글
// atts: 그 요청에 붙은 이미지(폰·PC 에서 보낸 것). prompt 에는 이미지 경로 목록이 빠져 있다

// 답장 보내기: reply 에 quote 를 싣고 turn_id 는 **답장할 요청 id**(없으면 bad_request). quote 가 없으면 예전처럼
//   turn_id 는 붙일 요청(없으면 세션의 마지막 요청). 세션에 넣는 글은 본문 맨 앞에 한 줄
//   `답장: #12 결과 「발췌」`(요청이면 `요청`) + 빈 줄 — Claude 도 어느 요청·결과를 두고 하는 말인지 안다.
//   발췌는 PC 가 넣을 때 자기 기록에서 만든다(폰이 보낸 글을 믿지 않는다).

// Reply — 폰 답 하나
{ "rid":"…", "text":"…", "state":"confirm|delivering|delivered|handled|rejected", "note":"…", "at":"ISO", "atts":["<id>",…],
  "quote":{"seq":12,"part":"response","text":"…"}|null }   // 답장이면 대상. chat.replies 는 요청으로 잡힌 답을 빼고 준다(거절만 남음)
```

`markdown` 은 PC 앱의 문서 보기와 같은 문서다(요청 · 작업 요약 · 응답 · 이어진 요청 · 작업 과정 · 정보).

## 5. 푸시 — 내용 없는 알림

- 폰(로그인 상태)이 `POST /v1/relay/push-ticket` → `{"data":{"ticket":"pt1.…","expires_at":"…"}}`.
  티켓 = 서버 키로 봉인한 `{사용자, 만료(7일)}`. 서버는 티켓을 저장하지 않는다.
- 폰이 `hello` 로 티켓을 PC 에 넘긴다.
- 새 결과가 생기면 PC 가 `POST /v1/relay/push {"ticket":"…"}` (인증 헤더 없음).
  서버는 티켓을 풀어 그 사용자의 기기에 **"AI 작업 · 새 결과가 도착했어요"** 만 보낸다(기기 언어로).
  데이터 `{"type":"ai_relay"}`. 세션 이름·요약은 알림에 싣지 않는다.
- 제한: 사용자당 15초에 1회(3회까지 몰아 쓰기) · 하루 300회. 티켓이 틀리거나 만료면 `401 ticket_invalid`.

## 6. PC 쪽 약속

- 폰에 보내는 모든 글은 수집 때 이미 가린 값(비밀 토큰 가림 · 길이 제한)이다.
- 폰이 부를 수 있는 것은 §4 표뿐이다. 파일 읽기·명령 실행 같은 통로는 없다. 폰의 세션 관리(§4-2)는 PC 의 DB 에만 쓰고 폰에는 사본을 두지 않는다.
- 폰 답은 기존 전달 경로(채널 → 꺼진 세션 이어가기 → 거절)와 검사(기기 허용 · 세션 차단 · 데스크톱 확인)를 그대로 거친다.
- 페어링 정보(PC 정적 개인키 · 방 비밀 · 기기별 psk)는 앱 데이터 폴더의 `relay-identity.json`(본인만 읽기 600)에 둔다.
