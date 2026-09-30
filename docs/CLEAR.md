# /clear 로 끝난 대화 · 이력 보관 · 이력 검색 (0.9.0 설계·조사)

## 1. 조사로 확정한 사실

| 사실 | 근거 |
|---|---|
| Claude Code 는 /clear 하면 옛 세션에 `SessionEnd(reason=clear)` 를 보내고 몇십 ms 뒤 **새 세션 ID** 로 `SessionStart(source=clear)` 를 보낸다. 옛 세션 기록 파일은 그대로 남는다. | **실측**: 이 앱 DB `hook_event` 의 clear 쌍 23건(옛 ID→새 ID 간격 ≈50ms), 옛 세션 transcript 존속 확인. 새 세션 기록 첫머리에 `/clear` 로컬 명령 줄이 남는다 |
| 옛 대화는 `/resume` 으로 다시 열 수 있다. 같은 프로세스에서는 /rewind 목록 맨 위에 `/resume <id> (previous session)` 항목이 뜬다(프로세스를 끝내거나 다른 세션을 재개하면 사라짐). | 공식 문서 checkpointing |
| /rewind 체크포인트는 대화와 함께 저장되고(`~/.claude/file-history/<세션>/`), 세션이 마지막으로 스냅샷을 저장한 지 `cleanupPeriodDays`(기본 30) 뒤 정리된다. 대화 기록(`projects/**/<세션>.jsonl`)도 같은 기간 뒤 정리되고 그 뒤엔 `/resume` 목록에서 사라진다. | 공식 문서 checkpointing · claude-directory "Cleaned up automatically" · settings-reference `cleanupPeriodDays`(최소 1, 기본 30). 이 PC 는 값을 설정하지 않아 기본 30일 |
| 정리는 세션을 시작할 때 도는 백그라운드 작업이라 **정확한 삭제 시각은 알 수 없다.** | 공식 문서 문구 ⇒ 유예를 둔다(**추정에 기댄 부분**) |
| Codex: `/clear` 는 "터미널을 지우고 새 채팅", `/new` 는 "새 채팅 시작". 옛 스레드는 저장된 채팅으로 남아 `/resume` 된다. 종료 훅·기간 정리 규칙이 없다. | **추정**(codex 0.157.1 바이너리의 슬래시 명령 설명 · 기록 구조). /clear 와 /new 를 구별하는 신호도, "되돌릴 수 없는 시점"도 없다 |

## 2. 판정 규칙 (보수적: 확실할 때만 지운다)

- **끝난 대화** = Claude Code 세션의 `SessionEnd(reason=clear)` (`lifecycle::on_clear`). 훅이 설치되어 있어야 감지된다. Codex 는 감지하지 않는다 → **자동 삭제 없음**(후속 과제).
- **삭제 예정 시각** = max(/clear 시각, 세션 마지막 활동, 대화 기록 파일 수정 시각) + max(30, `cleanupPeriodDays`)일 + **유예 7일**.
  `cleanupPeriodDays` 는 `~/.claude/settings(.local).json`·프로젝트 `.claude/settings(.local).json` 중 가장 큰 값. 더 짧게 설정돼도 30일을 쓴다(오래 기다리는 쪽이 안전). 조직 관리 설정은 읽지 않는다(한계).
- 만료 검사(1분마다, `lifecycle::sweep`)는 아래를 **전부** 만족할 때만 지운다: 상태 `purge` · 예정 시각 지남(저장값과 재계산 둘 다) · 후속 세션(`cleared_to`) 프로세스 없음 · 자신도 실행 중 아님 · 고정·별표 없음 · 진행 중 요청·전달 대기 말 없음(`archive::delete_sessions` 가 다시 검사).
- **되살림**: /clear 뒤 그 세션에서 새 요청·입력·재개 훅이 오면 끝난 표시를 푼다(예약 취소). 이력 보관도 같이 풀린다(다시 /clear 되면 보관이 이어진다).
- 지울 때는 요청 ID 표식(`turn_deleted`)을 남겨 원본을 다시 읽어도 되살아나지 않는다. 로그는 `purge_log`(시각·세션 수·요청 수만, 내용·이름 없음).

## 3. 기본값 판단 (사용자가 바꿀 수 있음)

무응답 기본은 **삭제 예약**으로 했다. 이유: 사용자의 요구가 "clear 하면 내역이 날아간 것이니 자동 삭제"이고, 실제 삭제는 Claude Code 자신이 되돌리기용으로 두는 기간(30일)+7일 뒤에 일어나며, 그전에 결정 안내(토스트·사이드바 띠·세션 위 막대)·취소·이어 쓰면 자동 취소가 있어 되돌릴 수 없는 손실 위험이 낮다.
그래도 이 앱 DB 는 그동안 "장기 보관소"였으므로 **소급하지 않는다**: 이번 업데이트 전에 /clear 된 세션(실제 DB 23개)은 `ask`(처리 미정) — 흐리게만 표시하고 자동 삭제하지 않는다.
설정 → "/clear 로 끝난 대화"에서 기본을 `삭제 예약`(기본) · `이력으로 보관` · `매번 묻기(정할 때까지 자동 삭제 없음)` 로 바꾼다.

## 4. 스키마 v11 (마이그레이션)

`session` 에 열 추가(기존 데이터 불변): `cleared_at` `cleared_to` `clear_state`(purge|keep|ask) `purge_at` `clear_asked` `kept_at` + 인덱스 + 표 `purge_log`.
마이그레이션 전 `inbox.db.bak-v10`(VACUUM INTO, 본인 전용) 사본을 한 벌 남긴다. 옛 `end_reason='clear'` 세션은 `cleared_at=ended_at`, `clear_state='ask'`, `clear_asked=1`.
실제 DB 복사본에서 확인: 131 세션·1,597 요청 그대로, 23개만 표시, integrity ok.

## 5. UI

- 사이드바: 끝난 대화는 투명도 0.52·회색조·빗금 배경 + "끝남 · 삭제 예정 N일 후 / 이력 보관 / 처리 미정". 새 탭 **이력**(이력 보관 세션만, 전체·안 읽음·배지에서는 빠짐). 결정 대기가 있으면 "정하기" 띠.
- 대화 화면: 끝난 대화는 본문을 흐리게(마우스를 올리면 선명) + 위 막대에서 이력으로 보관 / 삭제 예약 / 그대로 두기.
- /clear 감지 시 토스트("정하기") → 결정 창(개별·일괄). 설정에 기본 동작·현황·정리 창.
- 폰(RELAY): 세션 항목에 선택 필드 `ended`({cleared_at,state,purge_at,asked}) 추가(옛 폰은 무시). 폰 목록에는 이력 보관 세션도 그대로 나온다. **폰 화면 변경은 코노티 앱 쪽 후속 과제**.

## 6. 대화 이력 검색 (채팅 모드 — 모델은 이 컴퓨터의 구독 CLI, 0.9.2)

- 위치: 사이드바 검색줄 옆 이력 아이콘 / ⌘⇧H. Claude Code·Codex 세션에 아무것도 전달되지 않는 별도 채팅.
- 흐름(`history.rs`): 질문 → 날짜 표현(어제·지난주·3일 전·9월 25일…)·검색어(한글 2글자 조각, BM25 비슷한 점수, 세션당 4건 상한) 로 **이 PC 안에서** 요청·결과 조각을 찾음 → 조각(요청 400자·결과 700자, 최대 16k자)만 모델에 전달 → 답 + `[n]` 인용 출처(누르면 그 대화로). 삭제된 세션은 찾지 못하고, 끝난 대화·이력 보관은 태그로 구분.
- 음성 모드의 과거 대화 조회는 코노티 폰↔PC 중계의 `sessions`·`chat` 조회로, 이 앱은 모델을 부르지 않는다.
- 동의·상한: 설정에서 **켜고 + 외부 전송 동의**해야만 호출(0.9.0 의 OpenAI API 동의는 넘겨받지 않고 다시 묻는다 — 저장값 `2` 만 유효). 하루 100회 상한(요청 태그 제안과 합산) · 호출마다 90초(태그 제안 60초) 제한. 동의 전에도 "모델 없이 찾기"(외부 전송 없음)는 된다.

### 모델 호출 (`llm.rs`, 0.9.2 — API 키 방식은 없앴다)

각 사용자의 컴퓨터에 설치·로그인된 **구독 CLI** 로 부른다. API 키·`openai-key` 파일·`OPENAI_API_KEY` 는 더 읽지 않는다(이전 버전이 남긴 키 파일은 설정에서 지울 수 있다 — 앱이 자동으로 지우지 않는다).

| | Claude Code | Codex |
|---|---|---|
| 감지 | `claude` 실행 파일(PATH·표준 설치 위치·로그인 셸) · `claude auth status --json`(`loggedIn`·`authMethod`, 이메일·조직은 읽지 않는다) | `codex` 실행 파일 · `codex login status`("Logged in using ChatGPT" / "…API key" / "Not logged in") |
| 호출 | `claude -p --safe-mode --tools "" --no-session-persistence --output-format text --model M --system-prompt <고정 지침>` | `codex exec --ephemeral --ignore-user-config --ignore-rules --skip-git-repo-check -s read-only --color never -C <빈 폴더> -m M -c model_reasoning_effort="low" -o <출력 파일> -` |
| 기본 모델 | `claude-haiku-4-5-20251001`(저렴·빠름 — 설정에서 `sonnet` 등으로) | `gpt-6-luna`(`codex debug models` 에 있음 — 원래 요청대로 luna 우선) |
| 실측 지연(짧은 프롬프트) | 약 3.7초 | 약 5.6초 |

- **선택**: 설정 "사용할 서비스" = 자동(로그인된 것 중 Claude 먼저) · Claude · Codex. 고른 쪽이 로그인되지 않았으면 다른 쪽으로 몰래 넘어가지 않는다. 둘 다 없으면 "모델 없이 찾기"만 안내. Codex 가 API 키로 로그인돼 있으면 "구독이 아니라 API 요금"이라고 경고한다.
- **입력은 표준입력으로만**: 인자에는 모델 이름·고정 옵션·고정 지침만 있고, 대화 발췌·질문은 없다(`ps` 로 안 보인다). Codex 는 지침 칸이 없어 지침+본문이 한 덩어리로 표준입력에 간다.
- **격리(전권 우회 옵션은 어디에도 없다)**: 빈 작업 폴더(데이터 폴더 `llm-scratch/`, 700)에서 실행 · 도구 없음 · 세션 저장 없음 · `--safe-mode` 는 훅·MCP·CLAUDE.md·스킬·플러그인·개인 설정 전부 끔(구독 로그인은 유지 — 실측; `--bare` 는 OAuth 를 안 읽어 못 쓴다). `--safe-mode` 를 모르는 옛 Claude 는 `--strict-mcp-config --disable-slash-commands` 로 낮춘다. 앱을 띄운 Claude·Codex 세션의 `CLAUDE*`·`CODEX_*` 환경변수는 넘기지 않는다.
- **되먹임(재귀·오염) 방지**: 내부 호출에는 환경변수 `AI_INBOX_INTERNAL=1` 과 작업 폴더 `llm-scratch/` 두 표식이 붙는다. (1) 훅 서브커맨드 `hook`·`wake`·`channel` 은 표식을 보면 즉시 끝난다. (2) 수집기는 등록부(`~/.claude/sessions`)·대화 기록에서 작업 폴더가 `llm-scratch/` 인 세션을 사용자 세션으로 만들지 않는다. 그래서 가짜 세션·태그·요청이 생기지도, 이력 검색 대상이 되지도 않는다(시험: `internal_model_call_transcripts_are_not_collected`, `tests/internal_marker.rs`).
- **오류·폴백**: 시간 초과(프로세스를 죽인다)·로그인 필요·사용 한도·모델 없음·빈 답은 고정 문구로만 알린다 — **CLI 의 stderr 는 요청 내용을 되풀이하므로(실측) 오류 문구·로그에 옮기지 않는다.** 화면은 오류를 보이고 "모델 없이 찾기"로 다시 물을 수 있다.
- 시험: 가짜 CLI 스크립트로 통합 시험(stdin 전달·인자에 발췌 없음·표식 환경변수·타임아웃·오류 분류·출력 상한·모델 이름으로 옵션 끼워 넣기 차단). 실제 CLI 한 번씩: `cargo test live_llm_roundtrip -- --ignored --nocapture`(프롬프트 "OK 라고만 답해").
