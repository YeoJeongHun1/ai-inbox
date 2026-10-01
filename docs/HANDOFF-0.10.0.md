# 0.10.0 인수인계 — Windows 확인 끝, 남은 일

> 이 문서는 작업을 다른 컴퓨터(맥)로 넘기는 메모다. 일이 끝나면 지워도 된다.

## 지금 상태

- `v0.10.0` 은 **draft** 다(공개 전). 태그는 PR #1 머지 커밋 `5c32df9` 를 가리키고, 그 뒤에 쌓인 커밋은 없다.
- draft 자산 8개: mac `.dmg` · `.app.tar.gz` · `.sig` / Windows `x64-setup.exe` · `.sig` · `.msi` / `latest.json` · `SHA256SUMS.txt`.
- CI(macOS · Windows) 녹색. Windows 수정 1~7번과 리뷰 반영 5건이 모두 들어 있다.
- **공개하는 순간 `releases/latest/download/latest.json` 을 보는 0.8.0 사용자 전체에게 자동 업데이트 알림이 간다.** 되돌리기 어렵다.

## Windows 실기 확인 결과 (draft 설치 파일 기준)

| 항목 | 결과 |
|---|---|
| 설치 파일 SHA256 이 `SHA256SUMS.txt` 와 같다 | 합격 |
| 0.8.0 위에 덮어 설치(종료 코드 0), 대기 훅(`wake`)이 설치를 막지 않는다 | 합격 |
| `hook-status`: 빠진 이벤트 없음 · 옛 명령 없음 · `read_missing` false | 합격 |
| 죽은 `waiters` 표식 파일 정리(7개 → 살아 있는 세션 수만큼) | 합격 |
| "이어가기" 버튼이 셸 선택 메뉴(PowerShell · cmd · Git Bash)를 띄우고, PowerShell 로 세션이 이어진다 | 합격 |
| 끝난 세션에 폰에서 메시지 → 앱이 백그라운드로 `claude` 를 이어 실행, **검은 콘솔 창이 뜨지 않는다** | 합격 |

확인하지 못한 것:

- `claude.cmd`(npm 래퍼)만 설치된 환경의 이력 검색 — 확인한 PC 의 `claude` 는 `claude.exe` 라서 `.cmd` 경로를 타지 않았다. CI 시험만 통과.
- cmd · Git Bash 항목으로 붙여 넣었을 때의 접속.

## 남은 일 (우선순위 순)

### 1. 릴리스 노트 작성 → draft 에 넣기 → 공개 여부는 사람이 정한다

- 새 기능 요약 + 업데이트 때 동작(대기 훅이 정상 종료됨) + Windows 제약을 적는다.
- Windows 제약(README 와 같은 내용으로):
  - `claude.cmd` 만 설치된 환경에서는 여러 줄 메시지가 안내 문구로 막힌다.
  - Git Bash 식 경로(`/c/Users/…`)는 `C:/…` 와 다른 폴더로 집계된다.
  - 옛 npm `claude`(1.x, node + cli.js)는 여러 줄 호출이 막힌다.
  - `PATHEXT` 는 읽지 않고 exe · cmd · bat 로 정해 두고 찾는다.
- **공개(Publish)는 사용자가 직접 허락한 뒤에만 한다.**

### 2. 태그 — 폴더 규칙이 너무 넓다 (설계 검토 필요)

증상: 한 세션이 여러 주제를 오가도 **그 세션의 작업 폴더(git 최상위 이름)로 만든 프로젝트 태그가 모든 요청에 붙는다.** 그 폴더와 상관없는 질문(날씨 등)에도, 다른 레포를 다루는 요청에도 붙는다. 그러면 "한 세션 안의 여러 주제를 요청 단위로 구분한다"는 태그의 목적이 약해진다.

원인(코드 근거 `src-tauri/src/tags.rs` 의 `태그 정하는 순서` 주석, `docs/TAGS.md`):

1. `#태그` 직접 지정
2. 낱말 규칙
3. **작업 폴더 규칙** — git 최상위 폴더 이름으로 프로젝트 태그와 경로 규칙(`source='hook'`)을 자동 생성하고 붙인다. 내용은 보지 않고 세션의 cwd 만 본다.
4. 24자 이하의 짧은 말이면 직전 요청(12시간 안)의 태그를 이어받는다.
5. 없으면 미분류.

검토할 방향(어느 쪽이든 동작이 바뀌므로 `docs/TAGS.md` 와 시험도 같이 고친다):

- A. 폴더 규칙은 **그 요청이 실제로 그 폴더의 경로를 읽거나 고쳤을 때**(`turn_touch`)만 붙인다. 근거 없는 요청은 미분류.
- B. 폴더 규칙의 점수를 낮춰 낱말·경로 근거가 하나도 없으면 3점 기준(`태그로 인정하는 최소 점수`)에 못 미치게 한다.
- C. 지금처럼 두고 설정에서 "폴더 자동 태그 끄기" 옵션을 추가한다.

주의: 규칙을 바꿔도 지난 요청은 다시 계산하지 않는 게 현재 설계다(앞으로의 요청부터). 이 설계를 유지할지 같이 결정한다.

### 3. 이미 훅을 설치한 Windows 사용자에게 읽기 규칙을 자동으로 넣을지

첨부 이미지 폴더 읽기 허용 규칙(`Read(//c/…/attachments/**)`)은 새로 설치한 경우에만 들어간다. 0.8.0 에서 올라온 사용자는 옛 버전이 남긴 1회용 표식(`install.read_rule_once`)이 이미 있어 **설정의 "다시 설치"를 눌러야** 더해진다(상태의 `read_missing` 이 켜진다). 자동으로 넣을지 정해야 한다.

### 4. 정리

- 머지가 끝난 브랜치 `fix/windows-claude-spawn-tags`, 임시 브랜치 `test-build/win-fix`(테스트 빌드 워크플로 1개 커밋, `main` 에 없음) — 삭제해도 된다. 삭제 전 `test-build.yml` 이 더 필요하지 않은지 확인한다.

### 5. 알려진 한계(이번엔 고치지 않음)

- Git Bash 식 경로, 옛 npm `claude`(1.x), `PATHEXT` 미지원 — 위 릴리스 노트의 제약 항목과 같다.
- 업데이트 중 대기 훅 처리는 "강제 종료를 피하게 한 완화"이고, 실제 업데이트 과정을 끝까지 돌려 본 것은 설치 덮어쓰기 한 번뿐이다. 0.8.0 → 0.10.0 자동 업데이트(앱 안 업데이트 알림 경로)는 공개 후 실제 사용자에게서 처음 돈다.

## 새로 들어온 코드 위치(찾기 쉽게)

- 콘솔 창 숨김: `deliver.rs` 의 `no_console()` (Windows 에서만 `CREATE_NO_WINDOW`)
- `claude.cmd` 대응 · PATH 탐색: `deliver.rs` 의 `find_claude`, `npm_claude_exe`, `multiline_ok`
- 태그 Windows 경로: `tags.rs` 의 `slash_path` · `is_abs` · `has_drive` · `real_path`
- 시간 초과 때 자손 프로세스 종료: `llm.rs` 의 `win_job`(Job Object — 리뷰 반영으로 `kill_descendants` 방식을 대체)
- 업데이트 전 대기 훅 정리: `wake::release_all` / `wake::resume` / `wake::sweep_dead`
- 이어가기 셸 메뉴: `ChatView.tsx` 의 `ShellMenu`
- 단축키 표기: `src/keys.ts` 의 `kbd()`
- CLI 오류 글 디코딩: `text::cli_text`
