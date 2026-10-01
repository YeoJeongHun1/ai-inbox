# AI Inbox

**Claude Code·Codex 에 맡긴 요청을 메신저처럼 모아 보는 데스크톱 앱** — macOS · Windows

여러 세션을 동시에 돌리면 먼저 끝난 작업의 결과 보고가 다른 작업에 묻혀 못 보고 지나가곤 합니다.
AI Inbox 는 세션을 대화방처럼, 요청 하나를 말풍선 한 쌍(내 요청 → AI 결과)으로 보여 주고,
**아직 안 본 결과에만 파란 표시**를 붙여 놓칠 수 없게 합니다.

[English](#english)

## 무엇을 하나

- **세션 = 대화방, 요청 = 말풍선** — 텔레그램처럼 왼쪽에 세션 목록, 오른쪽에 요청과 결과가 이어집니다.
- **안 읽음 추적** — 결과 말풍선이 화면에 1초쯤 보이면 읽음. 안 본 결과는 목록 배지·메뉴 막대(macOS)·Dock 배지에 숫자로 남습니다.
- **확인 필요** — 결과가 질문으로 끝났거나(❓) 권한 승인을 기다리는 요청을 따로 모읍니다.
- **진행 중 표시** — 지금 도는 요청은 경과 시간과 "지금: Bash — 테스트 실행" 같은 마지막 동작을 실시간으로 보여 줍니다.
  백그라운드 에이전트를 기다리는 동안은 "백그라운드 작업 대기" 로 따로 표시합니다.
- **요청 하나 = 마크다운 문서 한 장** — 말풍선을 누르면 요청 · 답이 필요한 질문 · 작업 요약 · 계획 · 바뀐 파일 ·
  서브에이전트 · 응답 · 작업 과정 · 정보(모델·토큰·걸린 시간·문맥 크기)가 정리된 문서가 열립니다.
  복사하거나 `.md` 파일로 저장할 수 있습니다.
- **Codex 도 같이** — OpenAI Codex(CLI·데스크톱 앱) 세션도 같은 목록에 `Codex` 표시와 함께 모입니다. 훅 설치가 필요 없고,
  열려 있는 Codex 세션에도 입력창·폰에서 이어서 시킬 수 있습니다. 아래 "Codex" 절.
- **입력창 · 새 작업** — 채팅 아래 입력창에서 그 세션에 이어서 시킬 수 있고(Enter 보내기 · Shift+Enter 줄바꿈),
  **새 작업**(⌘N)으로 폴더와 에이전트(Claude Code · Codex)를 골라 백그라운드로 띄울 수 있습니다. 세션이 일하는 중이면 보낸 말은 기다렸다가
  끝나면 들어갑니다. 아래 "세션에 말 넣기" 절.
- **이미지 첨부** — 입력창·새 작업에 이미지를 5장까지 붙입니다(붙여넣기 ⌘V · 끌어다 놓기 · 버튼). 세션에는 이미지 파일 경로를 넘겨
  Claude 가 직접 열어 봅니다. 폰(코노티)에서 찍거나 고른 사진도 같은 방법으로 들어갑니다.
- **기록(⌘⇧F)** — 모든 세션의 지난 요청·요약·응답을 검색하고, 보낸 메시지(PC·폰)와 이미지를 모아 보고 지웁니다.
- **세션 정리** — 세션을 보관(목록·폰에서 빼고 검색에는 남김, 보관한 뒤 새 요청·결과가 오면 저절로 복귀)하거나 이 앱의 기록에서 지웁니다.
  기록 창의 세션 탭에서 "요청 1개 이하"·"오래 조용함"으로 걸러 한꺼번에 정리합니다. 지워도 Claude Code 원본 기록(`~/.claude`)은 그대로입니다.
  연결한 폰(코노티)에서도 길게 눌러 같은 관리를 합니다(PC 설정의 기기별 "기록 관리 허용", 데이터는 PC 에만).
- **알림** — 요청이 끝나거나 답을 기다리면 OS 알림 (앱을 보고 있을 때는 띄우지 않음, 짧은 요청 거르기·본문 숨기기 가능).
- **내장 SQLite** — 모든 기록이 앱 데이터 폴더의 `inbox.db` 한 파일에 쌓입니다. 서버가 필요 없습니다.
- **폰 연결 (선택, 코노티)** — [코노티](https://conoti.app) 앱의 홈 → **AI 작업** 에서 이 PC 의 세션·요청·문서를
  거의 그대로 보고, 답을 보내 **밖에서도 작업을 이어갑니다.** 내용은 PC 와 폰에서만 풀리는 **종단간 암호문**으로 오가고,
  코노티 서버는 암호문을 넘겨 줄 뿐 읽거나 저장하지 않습니다. 아래 "폰 연결" 절.

## 설치

[Releases](../../releases) 에서 받습니다.

| OS | 파일 |
|---|---|
| macOS (Apple Silicon · Intel) | `AI-Inbox_<버전>_universal.dmg` |
| Windows 10/11 (x64) | `AI-Inbox_<버전>_x64-setup.exe` 또는 `.msi` |

> **Apple·Microsoft 개발자 서명이 없는 앱입니다**(macOS 는 ad-hoc 서명).
> - macOS: 앱을 `응용 프로그램` 폴더로 옮긴 뒤 한 번 열고, 막히면 **시스템 설정 → 개인정보 보호 및 보안 → "그래도 열기"** 를 누르세요.
> - Windows: SmartScreen 에서 **추가 정보 → 실행** 을 누르세요.
>
> 받은 파일은 릴리스의 `SHA256SUMS.txt` 와 비교하고(`shasum -a 256 -c SHA256SUMS.txt`), 가능하면 빌드 출처도 확인하세요:
> `gh attestation verify "<받은 파일>" --repo YeoJeongHun1/ai-inbox`

처음 켜면 최근 7일(설정에서 변경)의 Claude Code·Codex 대화 기록을 읽어 채웁니다. 이때 이미 끝나 있던 요청은 읽은 것으로 들어갑니다.

### 훅 설치 (선택, 권장)

설정(⌘, / Ctrl+,) → **Claude Code 훅 → 훅 설치**.

훅 없이도 대화 기록 파일을 읽어 동작하지만, 훅을 설치하면 작업 완료 · 권한 대기 · 세션 종료를 즉시 받아 상태가 더 빠르고 정확해집니다.

- `~/.claude/settings.json` 의 `hooks` 에 `SessionStart · UserPromptSubmit · Notification · Stop · SubagentStop · SessionEnd`
  6개 항목을 **추가만** 합니다. 제거할 때도 명령어가 정확히 `"<경로>/ai-inbox" hook` 인 항목만 뺍니다 — 다른 훅과 설정은 건드리지 않습니다.
- 처음 바꾸기 전 상태를 `settings.json.ai-inbox-backup` 에 **한 번** 남기고 이후엔 덮어쓰지 않습니다. 파일 권한도 그대로 유지합니다.
- 앱을 디스크 이미지(DMG)에서 바로 연 상태에서는 설치하지 않습니다 — 나중에 경로가 사라져 훅이 깨지기 때문입니다.
- 훅은 `"<앱 실행 파일>" hook` 을 부릅니다. 이 명령은 훅 입력에서 필요한 칸(이벤트 이름·세션 ID·경로·알림 문구)만 골라
  앱 데이터 폴더에 작은 파일로 떨어뜨리고 끝납니다(수 ms). **프롬프트 본문은 저장하지 않고**, 아무것도 출력하지 않으며, 어떤 오류에도 0 으로 끝납니다.
- 이미 열려 있는 Claude Code 세션은 설정을 다시 읽을 때부터 훅을 부릅니다.
- Windows 에서는 Claude Code 가 훅을 Git Bash 로 실행한다는 전제입니다. **Windows 훅 동작은 아직 실기 검증 전입니다** — 훅 없이도 기록은 모입니다.
- Windows 제약(0.10.0 기준, CI 시험은 통과했지만 실기 검증 전인 것 포함):
  - 예약 전송·권한 모드 안내는 훅에 의존하므로 훅과 같은 조건입니다.
  - Codex 는 열림 여부를 알 수 없어 항상 대기열에 넣습니다. npm 래퍼 `codex.cmd` 는 지원하지 않습니다(`codex.exe` 필요).
  - `claude` 는 흔한 설치 위치를 먼저 보고, 없으면 `PATH` 에서 찾습니다. npm 으로 설치했으면 래퍼(`claude.cmd`) 뒤의 `claude.exe` 를 찾아 직접 부릅니다.
    npm 설치 스크립트를 건너뛰어(`--ignore-scripts`·`--omit=optional`) `claude.exe` 가 없으면 여러 줄 말을 넘기는 기능(세션에 말 넣기·새 작업·이력 검색·태그 제안)은 쓸 수 없다는 안내가 뜹니다.
  - 폴더 기반 자동 태그 제안은 Windows 경로(`C:\…`)도 받지만 실기 검증 전입니다. 로그인 시 자동 실행은 Windows 실기 검증 전입니다.

## 폰 연결 (코노티) — 선택

1. AI Inbox 설정 → **폰 연결** → **폰 연결하기 (QR)**.
2. 코노티 앱 → 홈 → **AI 작업** → **PC 연결하기** 로 QR 을 찍습니다(찍을 수 없으면 코드 복사 → 앱의 "코드 붙여넣기").
3. PC 에 뜨는 **새 기기 연결 요청** 에서 허용합니다. 이 폰에서 세션에 답을 보낼 수 있게 할지도 여기서 정합니다.

그 뒤로 폰에서 세션 목록 · 요청/결과 말풍선 · 요청 문서를 보고, 읽음 표시가 PC 와 맞춰지며, 답을 보내면 세션이 이어서 일합니다.
새 결과가 나오면 폰에 **내용 없는 알림**("새 결과가 도착했어요")이 갑니다. PC 가 꺼져 있으면 폰은 "PC 꺼져 있음" 을 보여 줍니다
(원본은 PC 의 `inbox.db` 하나뿐입니다).

**어떻게 서버가 못 읽나** — PC 와 폰이 Noise(`IKpsk2_25519_ChaChaPoly_SHA256`)로 통로를 맺고, 코노티 서버는 방 번호로
두 웹소켓을 짝지어 암호문만 넘깁니다(DB 에 쓰지 않음). 키 교환은 폰 카메라가 PC 화면의 QR 을 직접 읽어서 하므로
서버가 가운데서 키를 바꿀 수 없습니다. 규격: [`docs/RELAY.md`](docs/RELAY.md).

| 서버가 보는 것 | 서버가 못 보는 것 |
|---|---|
| 접속 IP · 접속 시각 · 메시지 크기와 횟수 · 푸시 횟수 | 프롬프트 · 응답 · 요약 · 세션 이름 · 프로젝트 경로 · 폰 답 |

폰 답도 데스크톱 입력창과 같은 방법으로 세션에 들어갑니다(아래 "세션에 말 넣기"). 다만 꺼진 세션·백그라운드 세션을
폰 답으로 이어서 실행하는 것은 설정의 "꺼진 세션은 백그라운드로 이어서 실행" 을 켰을 때만입니다.

**폰 답은 곧 원격 명령입니다.** 권한 확인을 끈 세션이라면 답 하나가 코드 실행으로 이어질 수 있어 겹으로 막습니다:
PC 에서 허용한 기기의 암호 통로로 온 답만(서버는 답을 만들 수 없음) · 기기별 "답 보내기 허용" · 세션별 "폰 답 막기" ·
"폰 답 받기 모두 멈춤" 스위치 · "데스크톱에서 확인한 뒤 전달" 옵션 · 길이·제어문자 검사 · 모든 답의 전달 기록(설정 → 최근 폰 답).

## 세션에 말 넣기 (입력창 · 새 작업 · 폰 답)

**실행 중인 세션에도 그대로 넣습니다 — 세션을 다시 시작할 필요가 없습니다.** 훅을 설치하면 세션마다 작은 대기 훅
(`ai-inbox wake`, Claude Code 의 [`asyncRewake`](https://code.claude.com/docs/en/hooks) 훅)이 하나씩 붙어 있다가,
세션이 쉬는 때에 보낸 말을 넘겨 Claude 를 깨웁니다.

| 세션 상태 | 전달 |
|---|---|
| 실행 중 · 쉬는 중 (터미널·백그라운드 모두) | 바로 들어간다 |
| 실행 중 · 일하는 중 · 권한 승인 대기 | 기다렸다가 하던 일이 끝나면 들어간다 |
| 꺼져 있음 | `claude --bg --resume` 으로 이어서 실행(요청이 끝나면 자동으로 멈춤 — 대화는 남는다) |
| AI Inbox 채널과 함께 실행 중 | Claude Code **채널**(연구 미리보기)로 바로 |

- 대기 훅은 SessionStart · Stop(요청이 끝날 때마다) · ConfigChange 에 걸립니다. 훅을 설치(업데이트)하면 앱이
  `settings.json` 의 **수정 시각만** 한 번 더 갱신해, 이미 열려 있던 세션들도 몇 초 안에 대기 훅을 띄웁니다.
  대기 훅이 사라진 세션에 보낼 때도 같은 방법으로 다시 잇습니다(내용은 바꾸지 않음, 20초에 한 번까지).
- 대기 훅은 대화형 세션과 `claude --bg` 세션에서만 돕니다. `claude -p`·SDK 실행에서는 바로 끝납니다(그쪽은 비동기 훅이 끝나길 기다리기 때문).
  세션이 끝나면 함께 끝나고, 네트워크를 쓰지 않으며, 한 세션에 하나만 돕니다(프로세스당 메모리 약 9MB).
- Claude 에게는 "사용자가 AI Inbox 에서 보낸 다음 지시 — 훅 오류가 아님" 안내와 함께 들어가고, 터미널에는 훅 알림 한 줄로 보입니다.
- **새 작업**은 고른 폴더에서 `claude --bg` 로 세션을 띄웁니다. 권한은 사용자의 Claude Code 기본 설정 그대로이고,
  승인이 필요하면 세션이 멈춥니다 — 채팅 위 "터미널에서 열기"(`claude attach <id>`)로 열어 승인하세요.
  Claude Code 가 아직 신뢰하지 않은 폴더에서는 시작하지 않습니다(터미널에서 한 번 열어 신뢰를 허용해야 합니다).
- 보낸 말은 대화 기록에 `AI Inbox 앱에서 보낸 사용자 메시지` 머리말을 달고 들어가며, 화면에서는 머리말을 떼고 "나 · AI Inbox" 로 보입니다.
- `claude` 는 셸을 거치지 않고 인자로 실행합니다(본문이 셸에 해석되지 않음). 앱이 Claude Code 안에서 실행됐더라도 그 세션의 환경변수는 넘기지 않습니다.
- 일하는 세션을 기다리는 말은 받은 뒤 3시간이 지나면 넣지 않습니다. 폰에서 보낸 말은 여기에 더해, 세션이 받을 수 있는데도 못 넣은 시간이 10분을 넘으면 넣지 않습니다(세션이 일하는 동안 기다린 시간은 세지 않음).

### Codex

`~/.codex/sessions/**/rollout-*.jsonl`(없으면 `$CODEX_HOME`)을 읽어 Codex 턴 하나를 요청 하나로 모읍니다. 설정 → **Codex** 에서 끌 수 있습니다.
하위 에이전트 스레드는 따로 세션으로 만들지 않고, Claude Code 대화를 Codex 로 가져온 사본은 건너뜁니다(이미 Claude 쪽에서 모았으므로).

| 세션 상태 | 전달 |
|---|---|
| 열려 있음 · 쉬는 중 | `codex queue` — Codex 대기열에 넣으면 열린 화면이 바로 가져가 처리(터미널 TUI 로 실측) |
| 열려 있음 · 일하는 중 | 기다렸다가 하던 일이 끝나면 대기열에 |
| 꺼져 있음 | `codex exec resume` 으로 이어서 실행 — 승인은 묻지 않고(never), 샌드박스는 그 세션이 쓰던 값(단 전권이던 세션은 `workspace-write` 로 낮춤) |

- 열려 있는지는 Codex 가 스레드를 열 때 거는 쓰기 잠금(`~/.codex/thread-writer-locks/<id>.lock`)으로 봅니다 — 잠금을 **잡지 않고** 상태만 묻습니다
  (macOS `F_GETLK`). Windows 에서는 알 수 없어 항상 대기열에 넣습니다(닫혀 있으면 다음에 열 때 처리). Windows 의 npm 래퍼(`codex.cmd`)로는
  여러 줄 말을 넘길 수 없어 네이티브 `codex.exe` 가 있어야 보낼 수 있습니다(모아 보기는 됩니다).
- **새 작업**을 Codex 로 고르면 `codex exec` 로 띄웁니다. Codex 설정에 `sandbox_mode` 가 없으면 그 폴더 안에서만 쓰기(`workspace-write`)로,
  승인은 묻지 않습니다 — 막히는 일은 실패로 끝나니 "이어가기"(`codex resume <id>`)로 터미널에서 이어 가세요.
- Codex 는 Git 저장소나 신뢰한 폴더가 아니면 exec 를 거절합니다 — 사용자가 고른 폴더·이미 그 폴더에서 돌던 세션이므로 `--skip-git-repo-check` 를 줍니다.

채널(선택): `claude mcp add --scope user ai-inbox -- "<앱 실행 파일>" channel` 후 `claude --dangerously-load-development-channels server:ai-inbox` 로 시작하면
일하는 중에도 채널로 바로 들어갑니다. 두 명령은 설정 화면에서 복사할 수 있습니다.

## 개인정보와 보안

AI Inbox 는 **원격 측정이 없고, 대화 내용을 밖으로 보내지 않습니다.** 네트워크를 쓰는 것은 둘뿐입니다.
**새 버전 확인**은 6시간마다 github.com 의 공개 릴리스 정보(`latest.json`)만 읽습니다(아무것도 보내지 않음 · 설정에서 끔).
업데이트는 사용자가 누를 때만 받고, 앱에 고정된 공개키로 서명을 확인한 뒤에만 설치합니다.
다른 하나는 사용자가 켠 **폰 연결**입니다. 그때만 `conoti.app`(또는 개발 서버 `dev.conoti.app`)과 TLS 로 통신하고,
다른 주소로는 보내지 않습니다(주소 고정·리다이렉트 금지). 서버로 가는 것은 방 번호 · 종단간 암호문 · 내용 없는 푸시 요청뿐입니다.

| 무엇을 | 어디서 | 어떻게 |
|---|---|---|
| 읽음 | `~/.claude/projects/**/*.jsonl` (대화 기록) | 요청·응답·도구 이름과 한 줄 요약·토큰 수 |
| 읽음 | `~/.claude/sessions/*.json` (살아 있는 세션) | 세션 이름·작업 중/대기 상태·프로세스 생존 |
| 읽음 | `~/.codex/sessions/**/rollout-*.jsonl` · `~/.codex/session_index.jsonl` | Codex 요청·응답·도구 이름과 한 줄 요약·토큰 수 · 스레드 이름 |
| 봄 | `~/.codex/thread-writer-locks/*.lock` | 잠겼는지만(열려 있는 세션인가). 잠금을 잡지 않는다 |
| 씀 | `~/.claude/settings.json` | **훅 설치/제거 버튼을 눌렀을 때만.** 원래 파일 권한 유지. 훅이 설치돼 있으면 실행 중인 세션을 다시 잇기 위해 **수정 시각만** 갱신. 훅과 함께 첨부 이미지 폴더 **읽기** 허용 규칙 하나(`permissions.allow` 의 `Read(//…/attachments/**)`) — 0.3.0 으로 올라오면 이 규칙만 한 번 더한다 |
| 실행 | `claude --bg` · `claude stop` · `claude agents --json` | **입력창에서 보내거나 새 작업을 시작했을 때만**(폰 답은 설정에서 허락했을 때만). 셸 없이 인자로 |
| 실행 | `codex queue` · `codex exec [resume]` · `codex --version` | 같은 조건. 셸 없이 인자로, 권한을 넓히는 옵션 없이. `~/.codex` 에는 앱이 직접 쓰지 않는다(codex 가 자기 대기열·기록에 쓴다) |
| 보냄(선택) | `conoti.app` | 폰 연결을 켰을 때만: 종단간 암호문(서버는 못 읽음)과 "새 결과" 푸시 요청. 첫 프레임에 앱 버전(옛 버전 차단용) |
| 읽음(선택) | `github.com` | 새 버전 확인(6시간마다, 설정에서 끔) · 사용자가 "업데이트"를 누르면 서명된 설치 파일 |
| 저장 | 앱 데이터 폴더 `inbox.db` · 폰 연결 비밀키 `relay-identity.json` · 보낸 이미지 `attachments/` | macOS `~/Library/Application Support/com.yeojeonghun.ai-inbox/` · Windows `%LOCALAPPDATA%\com.yeojeonghun.ai-inbox\` |

- 데이터 폴더는 본인만 열 수 있게(macOS/Linux `700`, DB `600`) 만듭니다. Windows 는 사용자 프로필 권한을 따릅니다.
- 보낸 이미지는 앱 데이터 폴더 `attachments/` 에만 둡니다(본인만 읽기). 같은 이미지는 한 벌만, 아주 큰 이미지는 긴 변 4096px 로 줄여 저장합니다.
  보내지 않고 뗀 이미지는 7일(폰은 1시간) 뒤 지우고, 보낸 이미지는 **기록**에서 직접 지울 때까지 남습니다.
- 도구 **출력**(파일 내용·명령 결과)은 저장하지 않습니다. 도구마다 한 줄 요약(명령 설명·파일 경로·검색어)만 남깁니다.
- 흔한 형식의 비밀값(API 키 · `Bearer`/`Basic` 인증 · JWT · `password=`/`"password":` · `postgres://user:pass@` · 개인키 블록 · 웹훅 URL 등)은
  저장 전에 `[가림]` 으로 바꿉니다. **완벽한 탐지는 아닙니다** — 문서를 `.md` 로 내보내 공유하기 전에 한 번 읽어 보세요.
  잠금 화면 알림에 내용이 뜨는 게 싫으면 설정에서 "알림에 응답 첫 줄 보이기" 를 끄세요.
- "이어가기" 로 복사되는 명령은 경로를 작은따옴표로 감싸 대화 기록 속 경로에 특수문자가 있어도 명령이 끼어들지 않습니다.
- 화면(WebView)은 엄격한 CSP 아래에서 돌고, 앱 밖 주소로 이동할 수 없습니다. 대화 기록 속 링크는 누를 때만 기본 브라우저로 열립니다(http · https).
  마크다운의 원격 이미지는 불러오지 않습니다.
- 앱이 파일을 쓰는 곳은 다음뿐입니다: 앱 데이터 폴더(DB·훅 이벤트·채널 전달 대기열), 훅 설치/제거 때의 `settings.json`(과 그 백업·임시 파일),
  저장 대화상자에서 **직접 고른 `.md` 파일**, "로그인하면 자동 실행" 을 켰을 때의 로그인 항목(macOS LaunchAgent · Windows 시작 프로그램 레지스트리).

취약점 제보는 [SECURITY.md](SECURITY.md) 를 보세요.

## 지우기

1. 설정 → Claude Code 훅 → **제거** (앱을 먼저 지우면 Claude Code 가 없는 파일을 부르며 훅 오류를 냅니다)
2. 앱 삭제
3. 데이터 폴더 삭제 (위 표의 경로)

명령줄로도 훅을 뺄 수 있습니다: `"<앱 실행 파일>" uninstall-hooks`
(macOS 실행 파일: `AI Inbox.app/Contents/MacOS/ai-inbox`)

## 개발

필요: Node 20+, Rust stable, (Windows) WebView2 · MSVC 빌드 도구

```bash
npm ci
npm run tauri dev            # 개발 실행
npm run tauri build          # 설치 파일 빌드
cd src-tauri && cargo test   # 수집 엔진·훅 설치·텍스트 처리 테스트 (먼저 npm run build 로 dist 생성)
```

| 경로 | 역할 |
|---|---|
| `src-tauri/src/ingest.rs` | 대화 기록을 증분으로 읽어 "요청 1건" 을 조립하는 수집 엔진 |
| `src-tauri/src/ingest_codex.rs` · `codex.rs` | Codex 기록 해석 · Codex 세션이 열려 있나(잠금) · 대기열·이어서 실행·새 작업 |
| `src-tauri/src/hook.rs` · `install.rs` | 훅 수신기 · `settings.json` 설치/제거 |
| `src-tauri/src/db.rs` · `api.rs` | SQLite 스키마 · 화면이 부르는 명령 |
| `src/` | React 화면 (`markdown.ts` 가 요청 문서를 만든다) |

진단용 명령: `ai-inbox ingest-once` 는 창 없이 수집만 끝까지 돌리고 요약을 출력합니다
(`AI_INBOX_DATA_DIR=<임시 폴더>` 를 주면 실제 데이터 폴더를 건드리지 않습니다 — 이 변수는 이 명령과 개발 빌드에서만 읽습니다).

## English

**AI Inbox** is a desktop app (macOS · Windows) that turns your Claude Code and OpenAI Codex sessions into a messenger-style inbox:
each session is a chat, each request is a pair of bubbles (your prompt → the AI's result), and results you haven't
seen yet are marked until you look at them. Click a result to open it as a clean Markdown document
(request, questions for you, work summary, plan, changed files, sub-agents, response, timeline, model/token stats)
that you can copy or save as `.md`.

- Reads Claude Code transcripts (`~/.claude/projects/**/*.jsonl`) and the live-session registry, and Codex rollouts (`~/.codex/sessions/**/rollout-*.jsonl`);
  stores everything in a local SQLite file. Codex needs no hooks: open sessions are detected by probing Codex's thread-writer lock without taking it,
  messages go to open sessions through `codex queue` (the open TUI picks them up right away) and ended sessions are resumed with `codex exec resume`.
- **No telemetry; conversation content never leaves the machine** except end-to-end encrypted over the opt-in phone link.
  An update check reads the public release manifest on github.com every 6 hours (sends nothing, can be turned off); updates are
  installed only when you click, after verifying the signature against a public key pinned in the app. The other network use is the opt-in **phone link**
  (Conoti app → Home → AI tasks): the phone browses this PC's sessions, requests and documents and can reply to continue a
  session. Everything travels **end-to-end encrypted** (Noise `IKpsk2_25519_ChaChaPoly_SHA256`, keys exchanged by scanning a QR
  code on this PC's screen); the Conoti server only pairs two WebSockets and forwards ciphertext — it stores nothing and cannot
  read or forge messages. Push notifications carry no content. Spec: [`docs/RELAY.md`](docs/RELAY.md). Keys live in a user-only file (600) in the app data folder.
  Replies are treated as remote commands and gated in layers (desktop-approved device, per-device and per-session switches,
  global pause, optional desktop confirmation, length/control-char checks, delivery log).
- **Images:** attach up to 5 images per message (paste, drag & drop, or button) from the desktop or the phone. They are stored once
  (content-addressed) in the app data folder and handed to the session as file paths that Claude opens with its Read tool; installing hooks
  also adds a single read-only allow rule for that folder. **Archive (⌘⇧F)** searches every past request/summary/response and lists sent
  messages and images for viewing or deletion.
- **Composer & new task:** type below any chat to continue that session — **including sessions already running in a terminal,
  no restart needed** — or start a new background task in a folder (⌘N). The installed hooks attach one small waiter per session
  (`ai-inbox wake`, a Claude Code `asyncRewake` hook on SessionStart/Stop/ConfigChange) that hands your message over when the session
  is idle; busy sessions get it when their current work ends; ended sessions are resumed with `claude --bg --resume`. Waiters never run
  for `claude -p`/SDK runs. `claude` is invoked with arguments only — never through a shell.
- Optional hooks (Settings → Install hooks) are *added* to `~/.claude/settings.json` (a one-time backup is kept; other hooks are untouched,
  only entries whose command is exactly `"<path>/ai-inbox" hook` are ever removed). Windows hook support is not yet verified on real hardware.
  The hook receiver keeps only event metadata — never prompt text — and always exits 0 silently.
- Common secret formats are masked before storage (best effort — review exports before sharing).
- Not notarized: on macOS use System Settings → Privacy & Security → "Open Anyway"; on Windows SmartScreen → More info → Run anyway.
  Verify downloads with `SHA256SUMS.txt` and `gh attestation verify`.

## 라이선스

[MIT](LICENSE) · Claude 와 Claude Code 는 Anthropic 의, OpenAI 와 Codex 는 OpenAI 의 상표이며, 이 앱은 두 회사와 관계없는 개인 오픈소스입니다.
