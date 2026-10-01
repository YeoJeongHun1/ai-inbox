# 0.10.1 draft 재빌드 — Windows 재점검 결과

**결론(Windows 기준): 공개 가능.** 앞 점검의 기능 결함 2건(꺼진 세션 이어 보내기·Codex 실행 파일)이 실제 설치본에서 고쳐진 것을 확인했고, 새로 생긴 결함은 없다. 클립보드 실패 안내는 실패 상황을 재현하지 못해 실기 확인을 못 했고(코드에는 있음), 폰 기능은 실기 확인 수단이 없어 CI 시험 통과로 갈음한다.

- 점검일: 2026-10-02 (KST)
- 환경: Windows 11 Pro (x64) · Claude Code 네이티브 `claude.exe` · Codex CLI npm 설치(0.145.0) · 훅 설치된 상태
- 대상: 릴리스 v0.10.1 **draft** 의 `AI-Inbox_0.10.1_x64-setup.exe` (main `458f6ce` 로 재빌드된 것)
- 화면 확인: `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=…` + Playwright `connectOverCDP`. 앱은 임시 작업 스케줄러 항목으로 띄웠고, 끝난 뒤 항목 삭제·디버그 포트 없이 탐색기로 다시 실행(포트 닫힘 확인)
- 메시지 보내기 시험은 스크래치 저장소의 **스크래치 테스트 세션 하나에만** 했다(무해한 산수 한 줄). 다른 세션에는 아무것도 보내지 않았다

## 결과 요약

| # | 항목 | 결과 | 근거 |
|---|------|------|------|
| 1 | 새 빌드 자산 | 합격 | `gh release view` — setup.exe·.sig·latest.json·SHA256SUMS.txt 모두 `updatedAt 2026-10-01T14:36:4xZ`. setup.exe SHA256 `3824d395…dacf3` = SHA256SUMS.txt 일치. 이전 draft 설치본(`41e1db1d…91729`)과 다름 |
| 2 | 서명·업데이트 파일 | 합격 | `AI-Inbox_0.10.1_x64-setup.exe.sig` 있음(440B), 내용이 `latest.json` 의 `windows-x86_64.signature` 와 같음. `latest.json` 있음(version 0.10.1, pub_date 14:36:42Z, windows·darwin 두 아키텍처 URL) |
| 3 | 설치 전 DB 백업 | 합격 | `inbox.db`(+`-wal`·`-shm`) 파일 복사 + SQLite 온라인 백업본(integrity_check ok)을 레포 밖에 남김 |
| 4 | 덮어 설치 (`/S`) | 합격 | 앱 강제 종료 6초 뒤 설치, 종료 코드 0·약 3초. 설치된 exe 해시 `67121416…`(직전에 깔려 있던 로컬 테스트 빌드) → `36da976c…`, 수정 시각 KST 10-01 23:36(= 14:36Z). 설치 전 떠 있던 대기 훅(`ai-inbox.exe wake`) 1개는 설치 뒤 사라짐 |
| 5 | 하단 버전·빌드 시각 | 합격 | 사이드바 아래 `v0.10.1 · 빌드 10-01 23:31`. CI 빌드 시각(14:31Z)을 **KST 로 바꿔 표시** — 앞 점검 문제 5 해결 |
| 6 | 훅 상태 | 합격 | `hook-status`: 6개 이벤트 설치, `missing_events` []·`wake_missing` []·`stale_command` null·`read_missing` false |
| 7 | 죽은 대기자 표식 정리 | 합격 | 없는 pid 로 만든 가짜 표식 1개가 앱 재시작 직후 지워짐. 살아 있는 대기자 표식은 남음 |
| 8 | ★꺼진 세션으로 앱 입력창 연속 3회 | **합격** | 같은 앱 실행에서 「세션 종료」 상태 세션에 세 번 보냄 → 세 번 모두 약 4초 만에 정답(`api_calls=1`, `status=done`, `hidden=0`), 보낸 말이 화면에 그대로 남음. 매번 입력창 안내는 「세션이 꺼져 있습니다 — 보내면 백그라운드에서 이어서 실행합니다」. `conoti_reply` 세 건 모두 `handled`, 남은 `conoti.bg.*` 표식 없음 — 앞 점검 문제 1 해결 |
| 9 | 숨겨졌던 앱 말 복원 | 합격 | 앞 점검에서 답 없이 끝나 `hidden` 이던 앱 발송 말(3건)이 `hidden=0` 으로 돌아와 보임(PARSER_VERSION 16 재수집) |
| 10 | 458f6ce — 뒤 말을 멈추지 않음 | 합격(일부 재현불가) | 아래 「458f6ce 확인」 참고. 연달아 두 말을 보내 두 번째가 첫 말 처리 중에 대기열로 들어가게 했을 때 두 말 모두 답을 받음. 「답 없이(API 0) 끝난 말의 유예 끝」 자체는 실기에서 만들 수 없어 단위 시험 통과로 갈음 |
| 11 | 이어가기 — PowerShell | 합격 | 메뉴에 PowerShell·명령 프롬프트(cmd)·Git Bash. PowerShell 을 고르면 「이어가기 명령을 복사했습니다」, 클립보드에 `Set-Location -LiteralPath '…'; claude --resume <id>` 가 들어감. 그 명령을 PowerShell 에서 실행 → 같은 세션으로 이어지고 바로 앞 두 요청의 답을 맞힘 |
| 12 | 클립보드 점유 시 복사 실패 안내 | 확인불가 | 다른 프로세스로 `OpenClipboard` 를 잡아 두면 PowerShell `Set-Clipboard` 는 실패하는데, 앱의 복사는 그 상태에서도 성공해 실패 경로로 가지 않았다. WebView 쪽에서 `invoke` 를 바꿔 실패를 흉내 내는 것도 막혀 있음(쓰기 불가 속성). 코드상으로는 `tryCopy` 가 false 면 `CopyFallback` 창에 명령을 보여 주는 경로가 있음(7448710) |
| 13 | Codex 실행 파일 | 합격 | 설정 → 훅·Codex 에 `codex 0.145.0 — …\vendor\x86_64-pc-windows-msvc\bin\codex.exe`. 같은 폴더의 `codex-code-mode-host.exe` 가 아니라 `codex.exe` 를 고름, 경로 구분자도 `\` 로 통일 — 앞 점검 문제 2 해결 |
| 14 | 한글 줄바꿈 | 합격 | 사이드바 「정하기」 한 줄(`white-space: nowrap`). 대화 이력 찾기(Ctrl+Shift+H)를 연 상태에서도 사이드바 탭 「전체·안 읽음·확인 필요·진행 중·이력」이 모두 한 줄, 이력 입력창 안내문도 잘리지 않음(기본 창 크기) |
| 15 | 설정 Ctrl+F | 합격 | Ctrl+F → 「설정 검색 (Ctrl+F)」 칸에 포커스, 「다시설치」 → 「훅 설치 · 다시 설치 · 제거」·「관련어: 다시 설치」 |
| 16 | 좁은 창(620px) | 참고 | 머리 단추가 한 줄에 다 들어가지 않음 — 「이어가기」「요청 20」 글자가 두 줄로 꺾이고 오른쪽 단추 일부가 잘리며 가로 스크롤이 생김. 0.11.0d 계열 수정이 이 draft 에 없다는 전제대로이며 공개 판단에는 넣지 않음 |
| 17 | 콘솔 창 깜빡임 | 합격 | 화면에 보이는 콘솔 창(`ConsoleWindowClass`·Windows Terminal·PseudoConsole)을 30ms 간격으로 60초씩 두 번 감시하면서 꺼진 세션으로 보내기(백그라운드 이어서 실행)와 「다시 감지」(claude/codex 버전 확인)를 실행 → 새 창 0개 |
| 18 | 폰 예약 · 데스크톱 확인 모드 | 확인불가(CI 합격) | 폰 조작 수단이 없음. 대신 main `458f6ce` 의 CI(`cargo test --locked`)가 macOS·Windows 모두 성공(Windows 279 passed / 0 failed). 관련 시험 `confirm_mode_waits_and_device_revocation_blocks_at_delivery`·`phone_schedule_waits_for_desktop_confirm_like_a_phone_reply`·`phone_send_waits_for_desktop_confirm_in_one_insert_and_pc_can_cancel_it`·`phone_can_edit_and_cancel_only_its_own_schedules`·`sched_add_requires_permission_and_is_idempotent_by_rid` 등 모두 ok |

## 458f6ce 확인

커밋 diff 를 읽어 확인한 요지:

- `conoti.rs`: 답 없이 끝난 말을 유예 뒤 `handled` 로 넘길 때, **같은 세션에 그 요청보다 뒤 요청(turn.seq 가 더 큼)이나 뒤에 전달된 말(state='delivered')이 있으면 `stop_background` 를 하지 않고** `conoti.bg.<세션>` 표식을 남겨 그 말이 끝날 때 멈춘다. 유예 시간은 숨은 설정값 `empty_result_grace_ms`(기본 60초, 10초~10분으로 제한)
- 유예가 끝나도 답이 없으면 `conoti_reply.note` 에 「답을 받지 못했습니다 — 세션이 응답 없이 끝났습니다. 다시 보내 보세요」(사용자가 직접 중단한 말 `interrupted` 는 제외). PC 말풍선도 앱·예약·폰 발송이고 API 0 이면 같은 문구
- `ingest.rs`: 앱 발송 말의 「끝남」 알림을 유예 뒤 한 번으로 미루고, 그 안에 답이 오면 그 답으로 한 번만 알림
- `codex.rs`: 이 PC 아키텍처가 경로에 든 실행 파일을 우선

실기 관찰: 연달아 두 말(A·B)을 1.5초 간격으로 보냄 → A 처리 중에 B 는 「보내는 중…」 대기 후 전달, A·B 모두 정답을 받았고 B 가 도는 동안 세션이 멈추지 않음. 두 `conoti_reply` 모두 `handled`(B 는 답이 생긴 뒤 다음 틱에), 남은 bg 표식 없음. 다만 이번 시험에서는 A 가 정상 답을 받았으므로 「A 가 답 없이 끝나고 유예가 끝나는 틱에 B 가 일하는 중」 경계는 실기에서 재현하지 못했다 — 이 경계는 새 단위 시험 `grace_end_of_an_empty_message_does_not_stop_a_later_one` 이 CI 에서 ok.

## 그 밖에 (공개 판단과 무관)

- 사이드바 맨 위 앱 이름 「AI Inbox」가 기본 창 크기에서도 두 줄(「AI / Inbox」)로 보임. 머리 단추 줄 폭 문제와 같은 계열로 보임
- 이 PC 에는 직전에 다른 점검용 로컬 빌드가 깔려 있었다. 그래서 교체 여부는 버전 표기가 아니라 exe 해시·수정 시각으로 판정했다
