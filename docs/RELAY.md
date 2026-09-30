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
| `hello` | `{name, app, ticket?, acct?}` | `{desktop, version, can_reply, can_manage, can_schedule, paired?:{pid, psk, room}}` — `paired` 는 페어링 모드에서만. `pid`(16B)·`psk`(32B)는 b64url. `can_manage` 는 0.5.0 부터, `can_schedule` 은 0.10.0 부터(없으면 false) |
| `sessions` | `{filter?: "all"\|"unread"\|"attention"\|"active"\|"archived", tag?: "<이름>"}` | `{items:[Session], unread, attention, active, can_manage, can_schedule, archived?, tidy_short?, tidy_idle?}` — `archived`(보관함)·뒤의 세 수는 기록 관리를 허용한 기기만(§4-2). `tag`(0.10.0, §4-4) |
| `chat` | `{sid, before?: seq, limit?: 1..60, tags?: ["<이름>"…], any?: bool}` | `{session: SessionHead, turns:[Bubble], has_more, replies:[Reply]}` |
| `turn` | `{id}` | `{id, sid, title, markdown, status, needs_input, prev_id, next_id}` |
| `read` | `{ids:[…]}` 또는 `{sid}` | `{}` |
| `reply` | `{sid, turn_id?, text, rid, atts?:[id…], quote?: "prompt"\|"response"}` | `Reply` — `quote` 는 메신저식 답장(아래) |
| `att` | `{rid, i, data}` | `{id, w, h, bytes}` — 이미지 한 장 올리기(§4-1) |
| `att_get` | `{id, size: "thumb"\|"view"}` | `{id, mime, w, h, data}` — 이미지 받기(§4-1) |
| `unpair` | `{}` | `{}` — PC 가 이 기기를 지우고 통로를 닫는다 |
| `manage` | `{op: "archive"\|"unarchive"\|"pin"\|"unpin"\|"delete", sids:[sid…]}` | `{n, deleted?:[sid…], skipped?:[이름…]}` — 세션 관리(§4-2) |
| `tidy` | `{kind: "short"\|"idle"}` | `{n}` — 정리 제안대로 한 번에 보관(§4-2) |
| `sched_add` | `{rid, sid, text, atts?, turn_id?, quote?, when, on_missed?, within_min?, busy?}` | `Sched` (+ `warnings:[문구]`) — 예약 만들기(§4-3) |
| `sched_warn_off` | `{sid, off?: bool(기본 true)}` | `{}` — 그 세션의 예약 권한 안내를 끄거나(`off:false` 로 다시) 켠다. `sched_add` 와 같은 권한(§4-3) |
| `sched_list` | `{sid?}` | `{items:[Sched], active, held, can_schedule}` — 걸려 있는 예약 + 최근 7일 끝난 것(§4-3) |
| `sched_edit` | `{id, rev, text, atts?, rid?, when, on_missed?, within_min?, busy?}` | `Sched` — 발사 전 예약 고치기, `rev` 가 다르면 `conflict`(§4-3) |
| `sched_cancel` | `{id}` | `{}` — 예약 취소(§4-3) |
| `sched_act` | `{id, op: "send"\|"drop"}` | `{}` — 받지 못해 대기 중인 예약을 지금 보내거나 버린다(§4-3) |
| `tag_list` | `{}` | `{items:[{n, c, minor, k}]}` — 전체 요청 태그(§4-4) |
| `tag_set` | `{turn_id, add?:[이름], remove?:[이름]}` | `{tags:[{n, c}]}` — 요청의 태그 바꾸기(§4-4) |

- `ticket` 은 §5 의 푸시 티켓. 폰은 연결할 때마다 새로 받아 넘긴다.
- `acct` 는 로그인 사용자 id 로 만든 `hex(SHA-256("conoti-ai-acct/1" ‖ user_id))[0..16]`. 같은 계정의 폰이 여럿이면
  PC 가 푸시를 계정당 한 번만 보내는 데 쓴다(서버에는 가지 않는다).
- **기기는 처음 연결한 코노티 계정(`acct`)에 묶인다**(0.5.1). 연결 모드의 통로는 `hello` 가 통과해야 다른 요청을 받는다(`hello_required`).
  `hello` 의 `acct` 가 묶인 계정과 다르거나 없으면 `{"err":"account"}` 로 거절하고 통로를 닫는다 —
  같은 폰에서 다른 계정으로 로그인하면 그 PC 를 볼 수 없다. 폰은 그 연결을 지운다(주인 계정으로 다시 로그인하면 새로 연결한다).
  계정 없이 페어링한 옛 기기는 처음 온 계정에 묶인다. (0.5.1~0.5.2 는 `owner` 로 묶인 계정을 알려 주었다 — 0.5.3 부터 싣지 않는다.)
  ⚠️ 이것은 **보안 경계가 아니다** — `acct` 는 폰이 스스로 알리는 값이다. 기기 키를 가진 폰(= 이미 허용한 폰)이 값을 꾸미는 것은 막지 못한다.
  목적은 정직한 앱에서 계정마다 연결을 나누는 것이고, 접근 통제는 기기 키 + PC 의 허용이 맡는다.
- 오류 코드: `bad_request` · `not_found` · `rejected` · `conflict`(0.10.0 — 예약 `rev` 충돌·이미 발사됨) · `unknown_method` · `hello_required` · `account` · `unpaired`(PC 에서 해제됨 — 폰은 저장된 연결을 지운다) ·
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


### 4-3. 예약 전송 (0.10.0) — 시각·내용은 PC 에만, 폰은 요청만

정한 시각에 세션에 말을 넣는다. **PC 의 AI Inbox 가 켜져 있을 때만** 전달된다(서버 저장 0 — §1 약속 그대로 · PC 가 꺼진 동안 폰에서 거는 예약은 없다). 정본 설명은 `docs/SCHEDULE.md`.

- **권한**: `sched_add`·`sched_edit`·`sched_cancel`·`sched_act` 는 기기별 **"예약 허용"**(`can_schedule`, 기본 끔 — PC 설정 → 폰 연결에서 켠다)과 `can_reply` 가 모두 필요하다.
  아니면 `rejected`. PC 의 "폰 답 받기 멈춤"이 켜져 있어도 `rejected`. `sched_list` 는 페어링된 기기면 볼 수 있다(읽기만). 보관한 세션의 예약은 기록 관리를 허용한 기기만 본다.
  `hello`·`sessions` 의 `can_schedule` 은 이 값 그대로다. `sched_add` 는 세션 차단(폰 답 막기)·기기 허용·전체 멈춤을 만들 때 한 번, **발사 순간 한 번 더** 본다 —
  그사이 기기를 해제했거나 예약 허용·답 보내기를 껐다면 발사하지 않고 알림만 간다(기기를 해제하면 그 기기가 건 예약은 PC 가 거둔다).
- **`when`** — 셋 중 하나:
  - `{"after_min": 30}` — 지금부터 N분 뒤(1~43,200).
  - `{"at": "2026-10-01T00:00:00Z", "tz": "Asia/Seoul"?}` — 절대 시각(RFC 3339). 폰이 자기 시간대로 풀어 보낸다. `tz` 는 표시용(모르면 UTC).
  - `{"local": "2026-10-01T09:00", "tz": "Asia/Seoul"}` — 현지 시각 + IANA 시간대. **PC 가 푼다**: 서머타임으로 없는 시각은 직후 첫 유효 시각으로 밀고, 두 번 있는 시각은 첫 번째로 잡는다.
  과거(5초 안 포함)이거나 366일 넘게 먼 시각은 `rejected`.
- **`on_missed`** — 앱이 꺼져 있었거나 PC 가 절전이라 시각을 놓쳤을 때(예정 시각보다 2분 넘게 늦으면): `run_once`(켜지는 대로 한 번 실행) · `skip`(실행하지 않고 알림만) · `within`(늦어도 `within_min` 분 안이면 실행, 1~1,440, 기본 60). 기본값은 `within` 60분.
- **`busy`** — 그 시각에 세션이 작업 중일 때: `interrupt`(바로 끼움 — 도구 사이에 읽힌다) · `after_work`(작업이 끝난 뒤) · `after_quiet`(방해금지 시간이 끝난 뒤). 없으면(`null`) PC 설정의 규칙을 따른다
  (예약 → 세션 규칙 → 태그 규칙 → 방해금지 시간대 → 전역 기본 `after_work` 순).
- **`rid`** — 폰이 만든 UUID(형식은 `reply` 와 같다). 같은 `rid` 로 `sched_add` 를 다시 보내면 **처음 만든 예약을 그대로** 돌려준다(두 번 만들지 않는다).
- **이미지**: 폰은 `att` 로 먼저 올리고(`rid` = `sched_add` 의 `rid`, §4-1) `sched_add.atts` 에 id 를 싣는다. PC 는 그 이미지를 예약에 묶어 **예약이 걸려 있는 동안은 1시간이 지나도 지우지 않는다**(발사되면 보낸 말이 이어받는다).
  `sched_edit` 로 새 이미지를 붙일 때는 `rid` 도 보낸다(이 기기가 그 `rid` 로 올린 것만). 다른 기기·다른 `rid` 의 id 는 `bad_request`. 예약의 이미지는 `att_get` 으로 미리볼 수 있다.
- **`Sched`**:

```jsonc
{ "id":"sc0123456789abcdef", "sid":"…", "name":"세션 이름", "text":"…", "quote":null|"prompt"|"response", "turn_id":null|42,
  "atts":["<id>",…],
  "at":"2026-10-01T00:30:00.000Z"|null,   // 다음 발사 시각(UTC). 발사된 뒤엔 null
  "kind":"once|after", "tz":"Asia/Seoul",
  "on_missed":"run_once|skip|within", "within_min":60|null, "busy":null|"interrupt|after_work|after_quiet",
  "state":"active|done|cancelled", "rev":1, "created_at":"ISO", "mine":true,   // mine = 이 기기가 만든 예약
  "run":null|{ "at":"발사 예정이었던 시각", "state":"…", "reason":"…"|null, "note":"…"|null } }
```

  `run.state`: `pending`(발사됨·판단 전) · `deferred`(바쁜 세션·방해금지·승인 대기 때문에 미룸, `note` 에 이유) · `fired`(세션에 넣는 중) · `delivered`(세션에 들어감) ·
  `handled`(그 말로 요청이 시작됨) · **`held`(세션이 받을 수 없어 자동으로 보내지 않고 대기 — 사용자가 보내기/버리기)** · `missed`(놓침 정책이 실행하지 않음) · `failed`(정책상 막음 — 허용 세션 목록 밖·기기 허용 꺼짐) · `cancelled`.
  `run.reason`(held): `ended`(세션이 꺼져 있음) · `terminal`(훅 없는 터미널) · `perm`(권한 승인 대기 10분 넘음) · `busy_limit`(3시간 넘게 못 넣음) · `stuck`(대기열에서 10분 넘게 못 받음) · `rejected` · `cap`(하루 발사 100건 한도). 폰이 번역한다.
- **`held` 와 알림**: 꺼진 세션·훅 없는 터미널·승인 대기에서 멈춘 세션의 예약은 **대기열에 넣지 않고** `held` 로 둔다. 알림은 **한 번**(발사할 때), 세션이 다시 받을 수 있게 되면 **한 번 더**(`ended`·`terminal`·`perm` 사유만) — 그 뒤엔 없다.
  자동 전달은 없다: `sched_act {id, op:"send"}` 는 폰 답과 같은 검사(기기 허용·전체 멈춤·세션 차단·**꺼진 세션은 PC 의 "이어서 실행" 설정이 켜져 있어야** — 아니면 `rejected` + 이유)를 통과해야 하고 PC 의 "데스크톱 확인" 옵션이 켜져 있으면 확인 대기로 들어간다.
  `op:"drop"` 은 항상 된다. 7일 동안 처리하지 않으면 PC 가 버린다.
- **오류**: `not_found`(없는 예약·세션) · `conflict`(`rev` 불일치 · 이미 발사됨 — 목록을 다시 받아 고치라는 뜻) · `rejected`(권한·정책·형식 밖 값의 설명 문구) · `bad_request`(형식).
  한도: 세션당 걸려 있는 예약 20개 · 전체 200개 · 하루 발사 100건.
- **`Session`·`SessionHead` 선택 필드**: `sessions` 항목과 `chat` 응답의 세션 머리(`session`)에 `sched_n`(걸려 있는 예약 수)·`sched_held`(그중 처리를 기다리는 수) — 두 곳이 같은 값이다. 옛 폰은 무시하고, 옛 PC 는 보내지 않는다(없으면 0 으로 본다).
- **권한 안내 필드**(선택): `sessions` 항목과 `Sched`(`sched_add`·`sched_list`·`sched_edit` 결과)에 `perm` — 세션의 마지막 권한 모드 문자열(`default`·`plan`·`acceptEdits`·`auto`·`dontAsk`·`bypassPermissions`, PC 가 훅으로 안 값 · 모르면 `null` = "확인 불가") · `sched_warn` — 예약할 때 제약·사전 준비 안내를 띄워야 하는가(전부 허용 모드가 아니고 사용자가 그 세션의 안내를 끄지 않았을 때 `true`). `perm == "bypassPermissions"` 는 승인 없이 실행되는 세션이므로 폰은 "예약 내용은 신뢰하는 것만" 한 줄만 고지하면 된다.
  폰이 띄울 안내 문구는 PC 화면(`ScheduleDialog`)과 같은 내용을 쓰고(`docs/SCHEDULE.md` §5-1), "다시 안 보기"는 `sched_warn_off` 로 PC 에 저장한다. **`bypassPermissions` 세션도 예약을 받는다.** 모드는 마지막 훅 값이라 세션 중에 바뀌면 달라질 수 있다. **`perm == null`(확인 불가)이어도 PC 는 `sched_warn: true` 를 보낸다** — 폰은 "확인하지 못했어요" 경고를 띄운다(`perm` 이 `bypassPermissions` 가 아니고 사용자가 끄지 않았으면 언제나 `true`). **필드 자체가 없는 것은 옛 PC 뿐**이며 그때만 안내를 숨긴다. 옛 폰은 새 필드를 무시하고, 옛 PC 는 필드를 안 보내며 `sched_warn_off` 에 `unknown_method` 를 돌려준다.
- **푸시(§5)**: 예약이 전달되지 못했을 때 PC 는 폰 앱이 닫혀 있을 때만 같은 푸시 통로로 `POST /v1/relay/push {"ticket":"…","k":"<종류>"}` 를 보낸다(20초에 한 번까지 · 내용 없음). 종류는 못 받는 예약 held·정책상 막힘 failed → `sched_held`, 다시 받을 수 있게 됨 back → `sched_ready`, 놓침 missed → `sched_missed`(§5).
  서버가 `k` 를 알면 종류별 문구(기기 언어)를 보내고, **옛 서버는 `k` 를 무시하고 일반 문구("새 결과가 도착했어요")를 보낸다**(§5).
  폰 앱이 열려 있으면 PC 가 `{"ev":"changed"}` 를 보내니 `sched_list` 를 다시 부르면 된다.

### 4-4. 요청 태그 (0.10.0)

- `tag_list {}` → `{items:[{n, c, minor, k}]}` — 전체 태그(이름·색 `#rrggbb`·작은 태그 여부·붙은 요청 수). 페어링된 기기면 누구나.
- `tag_set {turn_id, add?:[이름], remove?:[이름]}` → `{tags:[{n, c}]}`(바뀐 뒤 그 요청의 태그). `add` 는 직접 붙임(`manual`) — 없는 이름은 **새로 만들고**(색 자동), 있는 이름은 **대소문자 무시로 재사용**한다.
  `remove` 는 뗌(`off` — 자동 규칙이 다시 붙이지 않는다). 이름은 1~24자·줄바꿈 없음, 한 번에 add·remove 각 10개까지, 전체 태그는 300개까지. **기록 관리 허용(`can_manage`)이 필요**하고(아니면 `rejected`),
  PC 의 "폰 답 받기 멈춤"이 켜져 있어도 `rejected`. 숨긴 요청·보관 세션(권한 없음)은 `not_found`. 성공하면 `{"ev":"changed"}` 가 간다.
- `sessions {tag?}` — 그 이름(대소문자 무시)의 태그가 붙은 요청이 **하나라도 있는** 세션만. 없는 태그는 빈 목록. PC 가 **전체 요청 태그**로 거른다(세션 항목의 `tags` 는 여전히 대표 3개).
- `chat {sid, tags?:[이름…], any?:bool, before?, limit?}` — 요청을 태그로 거른다. 기본은 **모두** 가진 요청만, `any:true` 면 하나라도. 없는 이름은 모두-조건에서는 아무 요청도 통과시키지 않고(빈 결과),
  하나라도-조건에서는 무시한다(아는 이름이 하나도 없으면 빈 결과 — 전체가 나오지 않는다). `tags` 는 10개까지(넘으면 `bad_request`). 모델 제안(`ai`)·뗀(`off`) 표식은 걸러지지 않는다.
  모르는 인자는 옛 앱이 무시하므로 옛 PC 에서는 전체 대화가 온다 — 폰은 `hello.version` 으로 판단한다.

### 객체 모양

```jsonc
// Session — 세션 목록 한 줄
{ "id":"…", "name":"api-refactor", "named":true, "project":"my-app", "dir":"~/code/my-app",
  "branch":"main", "live":"busy|idle|null", "pinned":false, "model":"claude-opus-5-5",
  "turns":12, "unread":2, "attention":0, "active":1,
  "last_status":"done", "last_needs_input":false, "preview":"…", "preview_ai":true, "last_at":"ISO8601",
  "archived":false, "agent":"claude|codex", "ended":null }
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
- 예약 관련(0.10.0): 같은 요청에 선택 필드 `"k"` 를 더한다(§4-3). 허용값은 `sched_held` · `sched_ready` · `sched_missed` 뿐(`result` 는 k 없음과 같다, `sched` 는 서버의 별칭이며 PC 는 더 이상 보내지 않는다). 서버는 그 밖의 값을 `400 VALIDATION_ERROR` 로 거절한다 — 새 종류는 서버 먼저. 여러 알림이 한꺼번에 모이면 푸시는 하나만 보낸다(못 받음 > 놓침 > 다시 받을 수 있음). 알림 데이터에는 `{"type":"ai_relay","k":"…"}`.
  **옛 서버(운영)는 `k` 를 읽지 않고** 200 + 일반 문구("새 결과가 도착했어요")를 보내므로 무해하다. 새 서버가 모르는 값이라 400 을 돌려주면 PC 는 `k` 없이 한 번 더 보낸다(알림이 통째로 사라지지 않게). 계약 정본은 서버 쪽 `relay-push-kinds.md`.
- 제한: 사용자당 15초에 1회(3회까지 몰아 쓰기) · 하루 300회. 티켓이 틀리거나 만료면 `401 ticket_invalid`.

## 6. PC 쪽 약속

- 폰에 보내는 모든 글은 수집 때 이미 가린 값(비밀 토큰 가림 · 길이 제한)이다.
- 폰이 부를 수 있는 것은 §4 표뿐이다. 파일 읽기·명령 실행 같은 통로는 없다. 폰의 세션 관리(§4-2)는 PC 의 DB 에만 쓰고 폰에는 사본을 두지 않는다.
- 폰 답은 기존 전달 경로(채널 → 꺼진 세션 이어가기 → 거절)와 검사(기기 허용 · 세션 차단 · 데스크톱 확인)를 그대로 거친다.
- 페어링 정보(PC 정적 개인키 · 방 비밀 · 기기별 psk)는 앱 데이터 폴더의 `relay-identity.json`(본인만 읽기 600)에 둔다.


### 0.9.0 추가(선택 필드)

`sessions` 항목에 `ended` — `null` 또는 `{cleared_at, state:"purge"|"keep"|"ask", purge_at, asked}`. /clear 로 끝난 대화 표시용(옛 폰은 무시). 이력으로 보관한 세션도 폰 목록에는 그대로 나온다.

`sessions` 항목에 `tags` — `[{n: 이름, c: "#rrggbb"}]`(그 세션의 대표 태그 최대 3, 요청 태그에서 파생). `chat` 응답의 세션 머리에 `tags` — `[{n, c, k: 요청 수}]`(최대 12), 각 요청에 `tags` — `[{n, c}]`(사용자가 붙였거나 규칙이 붙인 것만 — 모델 제안은 받아들이기 전엔 싣지 않는다). 모두 선택 필드(옛 폰은 무시). 태그 편집·거르기는 0.10.0 의 `tag_list`·`tag_set`·`sessions{tag}`·`chat{tags}`(§4-4). 정본 설명은 `docs/TAGS.md`.

### 0.10.0 추가(선택 필드·새 메서드 — 암호 규약·`relay-vectors.json` 은 그대로)

`hello`·`sessions` 에 `can_schedule`, `sessions` 항목에 `sched_n`·`sched_held`·`perm`·`sched_warn`, 새 메서드 `sched_add`·`sched_list`·`sched_edit`·`sched_cancel`·`sched_act`·`sched_warn_off`·`tag_list`·`tag_set`, 새 오류 코드 `conflict`,
`sessions{tag}`·`chat{tags,any}` 인자, 푸시 선택 필드 `k`(`sched_held`·`sched_ready`·`sched_missed`), `chat` 세션 머리의 `sched_n`·`sched_held`. 옛 폰은 새 필드를 무시하고 옛 PC 는 새 인자를 무시하며 새 메서드에 `unknown_method` 를 돌려준다(폰은 `hello.version` 이 0.10.0 이상일 때만 부른다).
