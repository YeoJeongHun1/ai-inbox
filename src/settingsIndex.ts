/**
 * 설정 화면의 범주·항목 정의 — **렌더와 검색이 같이 쓰는 한 곳.**
 *
 * - `SETTINGS_BLOCKS` 의 순서·범주대로 설정 화면이 블록을 그린다(`Settings.tsx` 의 `BLOCK_VIEW` 가
 *   `Record<BlockId, …>` 라서 블록을 여기 추가하면 그리는 쪽이 빠졌을 때 타입 검사가 막는다).
 * - 블록과 그 안의 `subs` 가 검색 항목이 된다. 항목 `id` 는 화면 요소의 `data-set-id` 와 같다
 *   (조건부로만 보이는 요소면 블록으로 이동한다).
 * - `keywords` 는 화면에 안 보이는 동의어(영어·다른 말)다. 제목·설명은 화면 문구와 맞춘다.
 */

export type SettingsCat = "general" | "agents" | "history" | "tags" | "schedule" | "ai" | "phone" | "about";

export interface SettingsCatDef {
  id: SettingsCat;
  label: string;
  desc: string;
}

export const SETTINGS_CATS: SettingsCatDef[] = [
  { id: "general", label: "일반", desc: "알림 · 자동 실행 · 업데이트" },
  { id: "agents", label: "훅·Codex", desc: "Claude Code 훅 설치와 Codex 세션 모으기" },
  { id: "history", label: "기록·/clear", desc: "대화 기록 수집과 /clear 로 끝난 대화의 처리" },
  { id: "tags", label: "태그", desc: "요청마다 주제 태그와 자동 규칙" },
  { id: "schedule", label: "예약 전송", desc: "정한 시각에 세션에 말 보내기 · 방해금지 · 권한 정책" },
  { id: "ai", label: "이력 검색(AI)", desc: "예전 작업을 구독 AI 로 물어보기 · 동의" },
  { id: "phone", label: "폰 연결", desc: "코노티 앱 연결 · 기기별 권한 · 폰 답 전달" },
  { id: "about", label: "정보·데이터", desc: "버전 · 기록 통계 · 데이터 폴더" },
];

export interface SettingsItem {
  /** 화면 요소의 `data-set-id` */
  id: string;
  title: string;
  desc: string;
  keywords: string[];
}

export interface SettingsBlockDef extends SettingsItem {
  cat: SettingsCat;
  /** 블록 안의 하위 항목(검색 결과에서 그 자리로 바로 이동) */
  subs?: SettingsItem[];
}

export const SETTINGS_BLOCKS = [
  // ── 일반 ──
  {
    id: "notify",
    cat: "general",
    title: "알림",
    desc: "요청이 끝나거나 답을 기다리면 알림 (앱을 보고 있을 때는 띄우지 않음)",
    keywords: ["notification", "notify", "알람", "데스크톱 알림", "소리"],
    subs: [
      {
        id: "notify-body",
        title: "알림에 응답 첫 줄 보이기",
        desc: "끄면 제목만 — 잠금 화면에 내용이 안 보임",
        keywords: ["preview", "미리보기", "잠금 화면", "개인정보", "privacy"],
      },
      {
        id: "notify-min",
        title: "이보다 짧게 끝난 요청은 알리지 않기",
        desc: "모두 알림 · 30초 · 1분 · 2분 · 5분",
        keywords: ["threshold", "최소 시간", "짧은 요청"],
      },
    ],
  },
  {
    id: "autostart",
    cat: "general",
    title: "로그인하면 자동으로 실행",
    desc: "PC 에 로그인할 때 AI Inbox 를 띄웁니다",
    keywords: ["autostart", "startup", "login", "자동 시작", "시작 프로그램", "부팅"],
  },
  {
    id: "update",
    cat: "general",
    title: "업데이트",
    desc: "지금 버전 · 새 버전 알림 · 지금 확인 — 설치는 직접 고를 때만",
    keywords: ["update", "upgrade", "release", "새 버전", "최신", "릴리스", "github", "서명"],
    subs: [
      {
        id: "update-check",
        title: "새 버전이 나오면 알림",
        desc: "6시간마다 github.com 의 공개 릴리스 정보만 확인",
        keywords: ["자동 확인", "auto update"],
      },
      { id: "update-now", title: "지금 확인", desc: "새 버전이 있는지 바로 확인", keywords: ["check now", "업데이트 확인"] },
    ],
  },

  // ── 훅·Codex ──
  {
    id: "hooks",
    cat: "agents",
    title: "Claude Code 훅",
    desc: "작업 완료·권한 대기·세션 종료를 즉시 받고, 실행 중인 세션에 입력창·폰에서 말을 넣습니다",
    keywords: ["hook", "hooks", "claude code", "settings.json", "연결", "설치", "다시 설치", "재설치", "reinstall"],
    subs: [
      {
        id: "hooks-status",
        title: "훅 상태",
        desc: "상태 · 이벤트 · 명령어 · 설정 파일 · 이미지 읽기 · 마지막 수신",
        keywords: ["status", "이벤트", "event", "이미지 읽기 허용", "첨부 이미지", "permission", "마지막 수신"],
      },
      {
        id: "hooks-install",
        title: "훅 설치 · 다시 설치 · 제거",
        desc: "다른 훅·권한은 건드리지 않고 설치 전에 백업을 남깁니다",
        keywords: ["install", "uninstall", "remove", "제거", "삭제", "백업", "backup"],
      },
    ],
  },
  {
    id: "codex",
    cat: "agents",
    title: "Codex",
    desc: "OpenAI Codex 세션도 모아 보기 — 훅 설치가 필요 없습니다",
    keywords: ["openai", "codex cli", "codex exec", "gpt", "수집", "모아 보기"],
  },

  // ── 기록·/clear ──
  {
    id: "collect",
    cat: "history",
    title: "수집",
    desc: "처음 볼 때 가져올 기간 · 처음부터 다시 수집 (읽음·별표는 유지)",
    keywords: ["backfill", "rescan", "import", "가져오기", "재수집", "대화 기록"],
    subs: [
      { id: "collect-backfill", title: "처음 볼 때 가져올 기간", desc: "최근 1·3·7·14·30일", keywords: ["기간", "period", "days"] },
      { id: "collect-rescan", title: "처음부터 다시 수집", desc: "기간을 바꾸면 다시 수집 때 적용됩니다", keywords: ["다시 읽기", "reindex", "새로 고침"] },
    ],
  },
  {
    id: "clear",
    cat: "history",
    title: "/clear 로 끝난 대화",
    desc: "새로 /clear 된 대화는 삭제 예약 · 이력으로 보관 · 매번 묻기",
    keywords: ["clear", "삭제 예정", "삭제 예약", "이력 보관", "끝난 대화", "자동 삭제", "purge", "cleanupPeriodDays", "유예", "resume"],
    subs: [
      {
        id: "clear-tidy",
        title: "끝난 대화 정리…",
        desc: "/clear 로 끝난 대화를 골라 삭제 예약·보관을 정합니다",
        keywords: ["정리", "tidy", "cleanup", "일괄"],
      },
    ],
  },

  // ── 태그 ──
  {
    id: "tags",
    cat: "tags",
    title: "요청 태그",
    desc: "요청마다 주제 태그를 달아 나눠 봅니다 · 자동 태깅 규칙 · 모델 제안(선택)",
    keywords: ["tag", "tags", "label", "라벨", "분류", "태그 관리", "자동 규칙", "자동 태깅", "#태그", "프로젝트"],
  },

  // ── 예약 전송 ──
  {
    id: "sched",
    cat: "schedule",
    title: "예약 전송",
    desc: "쓴 말을 정한 시각(또는 N분 뒤)에 세션에 보냅니다 · 켜기/멈춤 · 세션이 작업 중일 때 기본",
    keywords: ["schedule", "scheduled", "timer", "예약", "나중에 보내기", "시계", "멈춤", "pause", "작업 중", "busy"],
    subs: [
      {
        id: "sched-dnd",
        title: "방해금지 시간 (시간대 규칙)",
        desc: "이 시간 안에 시각이 된 예약은 방해금지 시간이 끝난 뒤로 미룹니다 · 요일",
        keywords: ["dnd", "do not disturb", "quiet hours", "야간", "조용한 시간", "시간대", "요일"],
      },
      {
        id: "sched-rules",
        title: "세션·태그 규칙",
        desc: "세션마다 바로 · 작업 뒤 · 방해금지 뒤",
        keywords: ["rule", "세션 규칙", "태그 규칙", "우선순위"],
      },
      {
        id: "sched-perm",
        title: "예약 처리 정책 (권한)",
        desc: "모든 세션에 예약 · 허용 세션 목록에만 예약",
        keywords: ["permission", "권한", "allowlist", "허용 목록", "허용 세션", "승인 대기"],
      },
    ],
  },

  // ── 이력 검색(AI) ──
  {
    id: "history",
    cat: "ai",
    title: "대화 이력 검색 (구독 AI 사용)",
    desc: "이력 찾기(⌘⇧H)에서 예전 작업을 물어보면 발췌만 골라 구독 CLI(claude -p · codex exec)로 답합니다",
    keywords: ["history", "search", "ai", "llm", "이력 찾기", "단축키", "shortcut", "구독", "subscription", "api 키"],
    subs: [
      {
        id: "history-service",
        title: "감지된 서비스 · 사용할 서비스",
        desc: "자동(로그인된 것 · Claude 먼저) · Claude Code · Codex",
        keywords: ["provider", "감지", "다시 감지", "로그인", "cli"],
      },
      {
        id: "history-model",
        title: "Claude 모델 · Codex 모델",
        desc: "이력 검색 답에 쓸 모델 이름",
        keywords: ["model", "haiku", "sonnet", "opus", "gpt"],
      },
      {
        id: "history-consent",
        title: "발췌 전송 동의 · 모델로 답하기 켜기",
        desc: "대화 발췌가 선택한 구독 서비스의 서버로 전송 · 하루 사용 한도",
        keywords: ["consent", "동의", "철회", "개인정보", "privacy", "한도", "limit", "횟수"],
      },
    ],
  },

  // ── 폰 연결 ──
  {
    id: "phone",
    cat: "phone",
    title: "폰 연결 (코노티 앱)",
    desc: "코노티 앱에서 이 PC 의 세션·요청·문서를 보고 답을 보냅니다 · 종단간 암호화 · 서버",
    keywords: ["phone", "mobile", "conoti", "코노티", "모바일", "원격", "relay", "중계", "암호화", "e2e", "서버", "켜기"],
    subs: [
      {
        id: "phone-pair",
        title: "폰 연결하기 (QR)",
        desc: "5분 동안 한 번만 쓸 수 있는 QR · 코드 붙여넣기",
        keywords: ["qr", "pair", "pairing", "페어링", "다른 폰", "코드"],
      },
      {
        id: "phone-devices",
        title: "연결된 폰 · 기기별 권한",
        desc: "답 보내기 허용 · 기록 관리 허용 · 예약 허용 · 연결 해제",
        keywords: ["device", "기기", "권한", "permission", "연결 해제", "unlink", "disconnect"],
      },
      {
        id: "phone-options",
        title: "폰 알림·전달 옵션",
        desc: "새 결과 폰 알림 · 폰 답 받기 멈춤 · 데스크톱 확인 뒤 전달 · 꺼진 세션 이어서 실행",
        keywords: ["push", "푸시", "알림", "notification", "pause", "멈춤", "confirm", "background", "resume", "bg"],
      },
      {
        id: "phone-channel",
        title: "실행 중인 세션에 폰 답 넣기",
        desc: "Claude Code 의 채널 기능 · 등록 명령과 시작 명령 복사",
        keywords: ["channel", "채널", "mcp", "명령어", "command"],
      },
      { id: "phone-replies", title: "최근 폰 답", desc: "폰에서 보낸 답의 전달 상태", keywords: ["reply", "답장", "전달 상태", "기록"] },
    ],
  },

  // ── 정보·데이터 ──
  {
    id: "about-version",
    cat: "about",
    title: "버전",
    desc: "버전 · 빌드 시각 · DB 스키마 — 신고용으로 복사",
    keywords: ["version", "build", "schema", "about", "진단", "신고", "버그", "report", "정보"],
  },
  {
    id: "about-data",
    cat: "about",
    title: "기록 · 데이터 폴더",
    desc: "세션·요청 수와 DB 크기 · 데이터 폴더 열기 · 대화 기록 폴더",
    keywords: ["data", "folder", "db", "용량", "저장 위치", "경로", "path", "finder", "탐색기", "projects"],
  },
] as const satisfies readonly SettingsBlockDef[];

export type BlockId = (typeof SETTINGS_BLOCKS)[number]["id"];

/** 검색 대상 — 블록과 하위 항목을 펼친 것 */
export interface SettingsEntry extends SettingsItem {
  cat: SettingsCat;
  /** 이 항목이 속한 블록(하위 항목 요소가 화면에 없을 때 대신 이동할 곳) */
  block: BlockId;
}

export const SETTINGS_ENTRIES: SettingsEntry[] = SETTINGS_BLOCKS.flatMap((b): SettingsEntry[] => {
  const subs: readonly SettingsItem[] = "subs" in b ? b.subs : [];
  return [
    { id: b.id, cat: b.cat, block: b.id, title: b.title, desc: b.desc, keywords: [...b.keywords] },
    ...subs.map((s) => ({ ...s, keywords: [...s.keywords], cat: b.cat, block: b.id })),
  ];
});

export function catLabel(c: SettingsCat): string {
  return SETTINGS_CATS.find((x) => x.id === c)?.label ?? c;
}
