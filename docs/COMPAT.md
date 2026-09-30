# 운영 코노티 호환 (AI Inbox 0.10.0)

코노티 앱은 스토어 심사를 거쳐야 바뀌고(심사 없는 코드 푸시 패치는 Dart 코드만), AI Inbox 는 바로 배포할 수 있다.
그래서 **AI Inbox 가 스토어에 나간 코노티를 맞춘다.** 이 문서는 그 기준(최소 지원 버전)·폰이 실제로 보내고 읽는 것(계약)·
0.8.0 → 0.10.0 에서 바뀐 것의 판정·그것을 지키는 시험을 적는다. 규격 정본은 [RELAY.md](RELAY.md).

## 1. 호환 정책 — 최소 지원 버전

**AI Inbox 는 스토어에 배포된 코노티의 AI 작업(종단간 중계) 최소 지원 버전을 유지한다.**

| 항목 | 값 |
|---|---|
| 최소 지원 폰 | **코노티 1.5.0** (iOS 빌드 115 · Android 빌드 114) — AI 작업이 처음 들어간 버전. 1.4.x 이하에는 이 기능이 없다 |
| 지원하는 폰 | 1.5.0 · **1.6.0**(iOS 빌드 116 · 판매 중, Android 는 단계 출시) — 둘 다 아래 §2 계약으로 동작 |
| 다음 폰(미배포) | 예약·태그 화면이 든 코노티(개발 빌드) — 새 메서드는 `hello.version` 이 0.10.0 이상일 때만 부른다 |
| 중계 규격 | `join v:1` · Noise IKpsk2 — 암호 규약·`relay-vectors.json` 은 0.8.0 그대로 |

규칙:

1. **기존 rpc 응답의 필드는 이름·타입·의미를 바꾸지 않는다.** 필드는 추가만 하고, 추가한 필드는 폰이 몰라도 되는 것이어야 한다(§3).
2. 새 메서드·새 인자·새 오류 코드는 **새 폰이 스스로 꺼낼 때만** 나온다. 옛 폰은 아예 부르지 않는다.
3. 폰의 응답 형태가 바뀌어야만 풀리는 일은 폰 쪽 배포가 필요한 일로 분류해 알린다(§5) — PC 에서 옛 폰을 깨뜨려 해결하지 않는다.
4. 이 규칙은 시험이 지킨다(§4). 응답을 바꾸는 변경은 시험이 깨지고, 그때 §3 표와 `ADDED` 목록을 사람이 고친다.
5. 최소 지원 버전을 올리려면(옛 폰 정리) 그 폰이 스토어에서 내려가고 사용자 대부분이 새 버전이 된 뒤에만 한다.

## 2. 폰이 실제로 보내고 읽는 것 (계약)

기준은 코노티 **1.6.0(빌드 116)** 의 `ai_relay` 클라이언트다. 1.5.0(빌드 115)은 여기서 `quote`·`agent`·PC 별칭·`reply` 8초 재시도만 뺀 부분집합이다.

### 2-1. 폰이 보내는 rpc

| rpc | 폰이 보내는 값 `p` | 폰이 읽는 응답 `r` |
|---|---|---|
| `hello` | `{name, app, ticket?, acct}` (페어링 전엔 ticket 없음) | `desktop`(문자열) · `can_reply` · `can_manage` — `version` 등은 읽지 않는다 |
| `sessions` | `{filter}` — `all`·`unread`·`attention`·`active`·`archived` | `items[]` · `unread` · `attention` · `active` · `can_manage` · `archived?` · `tidy_short?` · `tidy_idle?` |
| `chat` | `{sid, before?, limit}` | `session` · `turns[]` · `has_more` · `replies[]` |
| `turn` | `{id}` | `id sid title markdown status needs_input prev_id next_id` |
| `read` | `{ids}` 또는 `{sid}` | (값을 읽지 않는다) |
| `reply` | `{sid, turn_id?, text, rid, atts?, quote?}` (`quote` 는 1.6.0 부터) | `rid text state note at atts quote` |
| `att` · `att_get` | `{rid, i, data}` · `{id, size: thumb\|view}` | `id w h bytes` · `id mime w h data` |
| `manage` · `tidy` | `{op, sids}` · `{kind}` | `n deleted? skipped?` · `n` |
| `unpair` | `{}` | (읽지 않는다) |

폰이 읽는 세부 필드 — 세션 행: `id name named project dir branch live pinned model turns unread attention active last_status last_needs_input preview preview_ai last_at archived agent`.
대화 머리: `id name named project dir branch live model cost_usd turns unread can_reply reply_block channel pinned archived agent`.
요청 말풍선: `id seq origin peer via_phone mid_turn prompt_at prompt slash status needs_input pending_bg summary response step ended_at duration_ms tool_calls files agents out_tokens model unread starred atts quote`.

푸시: 폰은 `hello` 에 푸시 티켓을 실어 보내고, 알림은 서버가 보낸다(`data.type = "ai_relay"`). PC 가 보내는 `{"ev":"changed"}` 를 받으면 목록을 다시 읽는다.
폰은 응답 오류를 `err`(코드)·`msg` 로만 다루고, 코드 중 `unpaired`·`account`(연결을 지움) · `rejected`·`not_found`·`bad_request`(문구·재시도 판단) 만 구별한다. 모르는 코드는 일반 실패다.

### 2-2. 폰 파서의 엄격도 (Dart 소스로 확인)

폰의 모델(`relay_models.dart`)은 **거의 전부 관대하다**: 문자열은 `_s`(문자열이 아니면 null), 숫자는 `_i`(숫자 아니면 0), 불리언은 `_b`(`true` 일 때만 참), 시각은 `tryParse`.
따라서 **모르는 필드는 무시되고, null·새 enum 문자열(`origin`·`status`·`state`)은 예외 없이 기본 처리**된다(`origin` 은 `peer`·`channel` 만 구별, 그 밖은 "나" · 모르는 답 `state` 는 원문을 그대로 표시).
예외를 낼 수 있는 곳은 아래 **강제 형변환** 뿐이다 — 이 자리의 타입은 유지해야 한다.

| 자리 | 폰 코드 | 조건 |
|---|---|---|
| `sessions.items`·`chat.turns`·`chat.replies`·`manage.deleted`·`manage.skipped` | `as List? ?? const []` | 리스트 또는 없음 |
| `chat.session` | `as Map?` | 객체 또는 없음 |
| `hello.desktop` | `as String?` | 문자열 또는 없음 |
| 응답 봉투 `id` | `is int` | 정수 |

## 3. 0.8.0 → 0.10.0 변경 판정

판정: **(가)** 무해 — 선택 필드·폰이 안 부르는 것 · **(나)** 위험 가능 — 폰 화면과 어긋날 수 있음 · **(다)** 확실한 비호환.
0.8.0 = 공개 기준(커밋 5ad691f). 0.9.x 는 내부 버전이라 함께 묶어 본다.

| 변경 | 옛 폰(1.5.0·1.6.0)에 미치는 영향 | 판정 |
|---|---|---|
| `hello`·`sessions` 응답에 `can_schedule` | 안 읽는 필드 | 가 |
| `sessions.items[]` 에 `ended`·`tags`·`sched_n`·`sched_held`·`perm`·`sched_warn` (`ended`·`perm` 은 null 일 수 있음) | 안 읽는 필드 — 폰은 필드별로 `_s`·`_i`·`_b` 로만 읽는다 | 가 |
| `chat.session` 에 `sched_n`·`sched_held`·`tags`, 요청에 `tags` | 안 읽는 필드 | 가 |
| 새 메서드 `sched_*`(6)·`tag_list`·`tag_set` · 새 인자 `sessions.tag`·`chat.tags/any` · 새 오류 `conflict` | 옛 폰은 부르지 않는다(`hello.version` 이 0.10.0 이상인 새 폰만 부른다) | 가 |
| `hello.version` 값이 `0.10.0` | 옛 폰은 읽지 않는다 | 가 |
| 요청 `origin` 에 새 값 `sched`(예약 전송으로 들어간 말) | 폰은 `peer`·`channel` 외를 "나"로 그린다(시험으로 고정) | 가 |
| 예약이 넣은 대기열 줄(`device='desktop'`) | 폰의 `replies` 는 `device <> 'desktop'` 만 싣는다 — 폰에 안 보인다 | 가 |
| 푸시 본문에 예약 알림용 선택 필드 `k` | 운영 서버는 본문을 `dict` 로 받아 `k` 를 읽지 않는다(200·일반 문구). 새 서버가 모르는 값으로 400 이면 PC 가 `k` 없이 한 번 더 보낸다. 폰이 받는 알림 데이터는 그대로다 | 가 (아래 주의) |
| 결과 푸시(예약 아님) | 본문 `{"ticket"}` 그대로 — `k` 없음 | 가 |
| 답 대기열 규칙(폰 말 10분 시계 · 받은 뒤 3시간 상한 · 승인 대기 정지) | **바뀌지 않았다** — 예약 전송 줄만 발사 시각 기준 3시간·승인 대기 10분을 따로 갖는다 | 가 |
| `manage delete`·`tidy` 가 걸려 있는 예약이 있는 세션을 남김(`skipped` 이름 목록) | 응답 모양 그대로. 폰은 "작업 중이거나 보낼 말이 남아 남겼어요"로 안내 — 예약이 남은 경우도 같은 문구 | 가 (문구만 다소 부정확) |
| **이력 보관 세션(/clear 뒤 보관)이 폰 목록엔 들어 있는데 배지 합(`unread`·`attention`·`active`)에서 빠짐** | 폰은 배지와 항목을 나란히 그린다 → 배지(3)가 항목 합(5)보다 작아짐 | **나 → 고침** |
| /clear 로 끝난 세션이 기본 설정에서 37일 뒤 이 앱의 사본과 함께 폰 목록에서도 사라짐 | 응답 모양은 그대로 — 목록에서 사라질 뿐(별표·고정·이어 쓴 세션은 지우지 않음) | 가 (동작 변경 — CHANGELOG 에 적음) |
| DB v10 → v14 마이그레이션(새 표·열, 단계마다 `.bak-v…` 사본) | 폰과 무관 — 0.8.0 에서 올라온 DB 로도 같은 응답을 내는지 시험한다 | 가 |
| Noise·프레임·`join`·`relay-vectors.json`·핸드셰이크·페어링 | 바뀌지 않았다(`git diff` 로 확인) | 가 |

**(다) 확실한 비호환은 없다.**

주의(가 이지만 알아 둘 것):

- 예약이 못 전달됐을 때 옛 **서버**(운영)는 예약 알림도 "새 결과가 도착했어요"로 보낸다 — 열어 보면 새 결과가 없다. 오류·크래시는 없고 문구만 부정확하다. 세분화된 문구는 서버가 `k` 를 알아야 하므로(서버 배포, 심사 불필요) 아래 §5 의 폰·서버 쪽 일이다.
- `hello.version` 은 폰이 새 기능을 켜는 유일한 신호다. 기능 목록(capabilities)은 없다 — 옛 폰은 새 기능을 몰라서 응답을 내려 줄 것도 없다(필드 추가뿐이라 다운그레이드 모드가 필요 없었다).

### 고친 것

`sessions` 응답의 배지 수(`unread`·`attention`·`active`)를 **폰용 개수**(`api::counts_on`, 이력 보관 세션 포함)로 낸다. PC 화면·트레이 배지는 그대로(`counts_of`, 보관 세션 제외).
0.9.0 이 이력 보관 세션을 PC 개수에서 빼면서 폰 목록과 어긋났다 — 0.8.0 과 같은 값으로 되돌렸다.

## 4. 시험

`src-tauri/src/relay/compat_driver.rs`(구 폰 시뮬레이터)·`compat_tests.rs` — `cargo test --lib compat`.

- **구 폰 시뮬레이터**: 폰 소스가 보내는 요청(§2-1)을 `Runner::call`(진짜 요청 경로)로 그대로 재현한다 — hello·sessions×5·chat(쪽 넘김)·turn·read·reply(일반·답장·재전송·오류)·이미지 올리기/받기·보관·고정·삭제·정리·`unpair`·알 수 없는 메서드.
  지어낸 세션·요청·기기를 **0.8.0 모양(v10) DB 에 넣은 뒤 0.10.0 이 마이그레이션**해서 쓴다 — 업그레이드한 사용자와 같다.
- **기준선(`tests/fixtures/compat_080_snapshot.json`)**: 같은 입력(`compat_driver.rs` 는 0.8.0 트리에도 글자 그대로 넣었다)을 **0.8.0 으로 실행해 얻은 응답**이다. `compat_v10_schema.sql` 은 그 DB 의 스키마.
- **기존 필드 불변**: 기준선의 모든 필드가 0.10.0 응답에 이름·값 그대로 있어야 한다(응답 시각 `at` 만 종류 비교). 새 필드는 시험의 `ADDED` 목록(위 §3 표의 필드)과 **정확히 같아야** 한다 — 목록 밖의 새 필드가 생기면 실패한다.
- **폰이 읽은 값이 같다**: 폰 Dart 모델의 파싱 규칙(§2-2)을 옮긴 파서로 두 응답을 읽어 같은지 비교하고, 강제 형변환 자리가 유지되는지 본다.
- 그 밖: 예약 출처 요청이 옛 폰 화면에서 무해한지 · 이력 보관 세션이 있어도 배지와 항목이 맞는지 · 세션 200개 응답 시간·크기 · 결과 푸시에 `k` 가 없는지.
- 변이 검사: 응답 필드 이름을 바꾸면 위 두 시험이 실패하는 것을 확인했다.

기준선은 **바꾸지 않는다**(0.8.0 이 낸 값). 다음 최소 지원 버전을 올릴 때 새 기준선을 그 버전 트리에서 만들어 교체한다 — 만드는 법: 그 트리에 `compat_driver.rs` 와 스키마·스냅샷을 쓰는 시험 하나를 넣고 실행한다.

## 5. 폰 쪽 필요 항목 (AI Inbox 가 고칠 수 없는 것)

| 항목 | 이유 | 배포 경로 |
|---|---|---|
| 예약 화면·요청 태그 편집·태그로 거르기(개발 빌드에 있음, 0.10.0 규격) | 옛 폰에는 화면이 없다 — 옛 폰에서 예약·태그 편집은 못 한다(PC 화면에서만) | 새 아이콘·에셋·플러그인·pubspec 변화가 없고(확인함) Dart 코드·문구뿐이므로 **코드 푸시(Shorebird) 패치로 가능**. 단, 같은 개발 트리에 든 iOS `Info.plist`(앱 언어 15개, 1.6.1)는 패치로 못 나가므로 **그 부분은 스토어 심사** |
| 예약 알림 세분화 문구(`k`) · 알림 탭 라우팅 | 폰 푸시 처리(앱)와 운영 서버의 `k` 지원이 함께 필요 | 서버: 배포(심사 없음) · 폰: 코드 푸시 패치 |
| 폰에서 PC 의 예약 권한 안내(`perm`·`sched_warn`) | 폰 화면 | 위와 같은 패치 |

새 폰 + 옛 PC(0.8.0)는 폰이 `hello.version` 으로 새 메서드를 부르지 않아 옛 동작 그대로다(폰 쪽 시험 · 이 문서의 계약 대조로 확인: 새 폰이 보내는 `sched_add`·`sched_edit`·`tag_set`·`chat{tags,any}` 인자는 RELAY.md §4-3·§4-4 와 일치).

## 6. 운영 폰 버전 기록 (2026-09-30 기준)

| 채널 | 버전 | 근거 |
|---|---|---|
| iOS 스토어 | 1.6.0 (빌드 116) 판매 중(READY_FOR_SALE) · 1.5.0(115) 도 판매됐던 버전 | 앱 스토어 커넥트 조회 |
| Android(Play) | 1.5.0(114) → 1.6.0 단계 출시(운영 기록상 20%) | 작업 기록(직접 조회 못 함) |
| 코드 푸시(Shorebird) | 운영 릴리스 1.6.0+116 (iOS·Android). 운영 패치는 아직 없음 | 작업 기록 |
| AI 작업 도입 | 1.5.0 부터(1.4.3 까지 없음) | 폰 소스 이력 |
| 답장(`quote`)·`agent` 표시·PC 여러 대 | 1.6.0 부터 | 폰 소스 이력 |
