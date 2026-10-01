import { invoke } from "@tauri-apps/api/core";

export type Status = "running" | "background" | "waiting" | "done" | "interrupted" | "stopped";

/** 세션을 만든 도구 */
export type Agent = "claude" | "codex";
export const AGENT_LABEL: Record<Agent, string> = { claude: "Claude Code", codex: "Codex" };

/** /clear 로 끝난 대화의 처리 — purge 삭제 예약 · keep 이력으로 보관 · ask 아직 안 정함(자동 삭제 없음) */
export type EndedState = "purge" | "keep" | "ask";

export interface Ended {
  cleared_at: string;
  state: EndedState;
  /** 삭제 예정 시각(state = purge) */
  purge_at: string | null;
  /** 사용자가 안내를 확인·결정했나 — false 면 결정 안내 대상 */
  asked: boolean;
}

/** 요청 태그 표식 — auto(규칙) · manual(사용자) · ai(모델 제안 — 받아들이기 전) */
export interface TurnTag {
  id: number;
  state: "auto" | "manual" | "ai";
}

export interface TagRule {
  id: number;
  kind: "path" | "keyword";
  pattern: string;
  source: "user" | "default" | "suggested" | "hook";
}

export interface TagInfo {
  id: number;
  name: string;
  color: string;
  /** 작은 태그(종류 표식) — 사이드바·대표 태그에서 뒤로 */
  minor: boolean;
  turns: number;
  rules: TagRule[];
  /** `#태그` 로 직접 지목한 횟수 */
  hashtag_uses: number;
  /** `#태그` 로 쓰는 이름을 낱말 규칙으로 승격하자는 제안(그 낱말) */
  promote: string | null;
}

export interface TagOverview {
  tags: TagInfo[];
  untagged: number;
  total: number;
  ai_pending: number;
}

export interface SessionTags {
  /** [태그 id, 이 세션에서 붙은 요청 수] */
  tags: [number, number][];
  untagged: number;
  total: number;
}

/** 태그로 거르기 — tags 중 하나라도(all=false) 또는 모두(all=true) 가진 요청, untagged 는 태그 없는 요청 포함 */
export interface TagFilter {
  tags: number[];
  untagged: boolean;
  all: boolean;
}
export const NO_FILTER: TagFilter = { tags: [], untagged: false, all: false };
export const filterActive = (f: TagFilter) => f.tags.length > 0 || f.untagged;

export interface FolderSuggestion {
  name: string;
  pattern: string;
  turns: number;
  depth: number;
  path: string;
}

export interface RuleCandidate {
  kind: "path";
  pattern: string;
  path: string;
}

export interface TagAiStatus {
  enabled: boolean;
  consent: boolean;
  /** 지금 쓰게 될 구독 서비스(마지막 감지 기준) — 없으면 null */
  active: LlmProvider | null;
  model: string;
  calls_today: number;
  daily_cap: number;
}

export interface TagAiPending {
  turn_id: number;
  session_id: string;
  session_name: string;
  seq: number;
  prompt: string;
  tag_id: number;
}

export interface SessionItem {
  id: string;
  name: string;
  named: boolean;
  project_dir: string | null;
  project_name: string | null;
  git_branch: string | null;
  live_status: string | null;
  pinned: boolean;
  cost_usd: number | null;
  model: string | null;
  turns: number;
  unread: number;
  attention: number;
  active: number;
  last_turn_id: number;
  last_status: Status;
  last_needs_input: boolean;
  last_origin: string | null;
  last_preview: string;
  last_from_ai: boolean;
  last_at: string | null;
  agent: Agent;
  ended: Ended | null;
  /** 대표 태그 id(요청 태그에서 파생 — 큰 태그·요청 수 순, 최대 3) */
  tags: number[];
}

export interface Turn {
  id: number;
  seq: number;
  origin: string | null;
  prompt_source: string | null;
  peer_name: string | null;
  prompt_at: string;
  prompt_text: string | null;
  slash_command: string | null;
  understanding: string | null;
  summary: string | null;
  response_text: string | null;
  status: Status;
  needs_input: boolean;
  ended_at: string | null;
  last_activity_at: string | null;
  duration_ms: number | null;
  active_ms: number | null;
  ttfr_ms: number | null;
  model: string | null;
  effort: string | null;
  api_calls: number;
  input_tokens: number;
  output_tokens: number;
  thinking_tokens: number;
  cache_create_5m: number;
  cache_create_1h: number;
  cache_read: number;
  web_search: number;
  web_fetch: number;
  context_tokens: number;
  tool_calls: number;
  files_changed: number;
  subagent_count: number;
  task_notifications: number;
  error_count: number;
  pending_bg: number;
  cwd: string | null;
  git_branch: string | null;
  plan_json: string | null;
  read_at: string | null;
  starred: boolean;
  last_step: [string, string | null, string | null] | null;
  /** 요청에 붙은 이미지 id (본문 끝의 경로 목록을 떼어 낸 것) */
  atts: string[];
  /** 요청 태그(뗀 것 제외) */
  tags: TurnTag[];
}

export interface SessionHeader {
  id: string;
  name: string;
  named: boolean;
  project_dir: string | null;
  project_name: string | null;
  git_branch: string | null;
  live_status: string | null;
  cc_version: string | null;
  model: string | null;
  cost_usd: number | null;
  lines_added: number | null;
  lines_removed: number | null;
  first_at: string | null;
  last_at: string | null;
  transcript_path: string | null;
  pinned: boolean;
  /** 보관한 세션 — 목록·폰에 없다(보관한 뒤 새 요청·결과가 오면 저절로 돌아온다) */
  archived: boolean;
  turns: number;
  unread: number;
  active_ms: number;
  output_tokens: number;
  input_total: number;
  api_calls: number;
  resume_command: string;
  /** Windows 에서만: 셸별 이어가기 명령(없으면 빈 배열) */
  resume_shells?: { shell: "powershell" | "cmd" | "bash"; command: string }[];
  /** 폰 답: 0 받음 · 1 막음 */
  conoti_mode: number;
  channel_live: boolean;
  /** 입력창에서 보내면: live 바로 · queue 일하는 중(끝나면) · approve 권한 승인 대기 · connecting 다시 잇는 중 · resume 꺼진 세션 이어서 실행 · terminal 대기 훅 미설치 */
  send_mode: SendMode;
  /** 실행 중인 백그라운드 세션을 터미널에서 여는 명령 */
  attach_command: string | null;
  agent: Agent;
  ended: Ended | null;
}

export interface CodexStatus {
  /** Codex 기록도 모으나(설정) */
  enabled: boolean;
  /** codex 실행 파일 — 없으면 새 작업·꺼진 세션 이어서 실행을 못 한다(모으기는 된다) */
  bin: string | null;
  version: string | null;
  sessions_dir: string;
  found: boolean;
  sessions: number;
  /** 지금 열려 있는 Codex 세션 수(Windows 는 null — 알 수 없음) */
  live: number | null;
}

export type SendMode = "live" | "queue" | "approve" | "connecting" | "resume" | "terminal";

/** 아직 요청으로 잡히지 않은 보낸 말 */
export interface OutboxItem {
  rid: string;
  text: string | null;
  state: "confirm" | "delivering" | "delivered" | "handled" | "rejected";
  note: string | null;
  at: string;
  from_phone: boolean;
  atts: AttMeta[];
  /** 답장이면 대상 요청 번호·부분 */
  quote: { seq: number; part: QuotePart } | null;
  /** 예약 전송이 시각이 되어 넣은 말 */
  sched?: boolean;
}

export type AttSize = "thumb" | "view" | "orig";

/** 첨부 이미지 한 장 (Rust attach::Meta) */
export interface AttMeta {
  id: string;
  mime: string;
  bytes: number;
  width: number;
  height: number;
  name: string | null;
  source: "desktop" | "phone";
  created_at: string;
}

export interface Page<T> {
  items: T[];
  has_more: boolean;
}

export interface TurnHit {
  turn_id: number;
  session_id: string;
  session_name: string;
  prompt_at: string;
  status: Status;
  origin: string | null;
  prompt: string;
  snippet: string;
  snippet_in: "prompt" | "summary" | "response" | "session";
  atts: string[];
  /** 보관한 세션의 요청 */
  archived: boolean;
  tags: TurnTag[];
}

/** 기록 창 "세션" 탭의 줄 */
export interface SessionRow {
  id: string;
  name: string;
  project_name: string | null;
  turns: number;
  first_at: string | null;
  last_at: string | null;
  archived: boolean;
  pinned: boolean;
  live: boolean;
  active: number;
  unread: number;
}

export type SessionScope = "visible" | "archived" | "all";

export interface SentMessage {
  rid: string;
  session_id: string | null;
  session_name: string | null;
  text: string;
  state: OutboxItem["state"];
  note: string | null;
  at: string;
  from_phone: boolean;
  device_name: string | null;
  result_turn: number | null;
  atts: AttMeta[];
}

export interface ArchiveImage extends AttMeta {
  used_in: { rid: string; session_id: string | null; session_name: string | null; at: string; text: string; from_phone: boolean }[];
}

export interface ArchiveStats {
  messages: number;
  images: number;
  image_bytes: number;
  /** 목록에 있는 세션 · 보관한 세션 */
  sessions: number;
  archived: number;
  /** 한 번에 정리해도 되는 것 — 요청 1개 이하 · 30일 넘게 조용함 */
  tidy_short: number;
  tidy_idle: number;
}

export interface ChatPage {
  session: SessionHeader;
  turns: Turn[];
  has_more: boolean;
}

export interface Step {
  seq: number;
  at: string | null;
  kind: "text" | "tool" | "task" | "compact" | "error" | "interrupt" | "continue" | "summary" | "ask";
  name: string | null;
  text: string | null;
}

export interface SubRow {
  agent_type: string | null;
  description: string | null;
  background: boolean;
  started_at: string | null;
  ended_at: string | null;
  duration_ms: number | null;
}

export interface HookRow {
  event: string;
  at: string;
  detail: Record<string, unknown> | null;
}

export interface TurnDetail {
  turn: Turn;
  session: SessionHeader;
  steps: Step[];
  tools: [string, number][];
  files: [string, number][];
  subagents: SubRow[];
  hooks: HookRow[];
  prev_id: number | null;
  next_id: number | null;
  next_prompt: string | null;
  next_at: string | null;
}

export interface Counts {
  unread: number;
  attention: number;
  active: number;
  /** 이력으로 보관한 세션 수(이력 탭) */
  kept: number;
  /** /clear 됐는데 아직 확인하지 않은 세션 수 */
  undecided: number;
}

export interface ClearRow {
  id: string;
  name: string;
  project_name: string | null;
  turns: number;
  last_at: string | null;
  ended: Ended;
}

export interface ClearOverview {
  purge: number;
  keep: number;
  ask: number;
  undecided: number;
  next_purge_at: string | null;
  retention_days: number;
  grace_days: number;
  default_state: EndedState;
  purged_sessions: number;
}

export type LlmProvider = "claude" | "codex";

export interface CliInfo {
  installed: boolean;
  version: string | null;
  /** true 로그인 · false 로그인 안 됨 · null 알 수 없음 */
  logged_in: boolean | null;
  login_kind: string | null;
  safe_mode: boolean;
}

export interface LlmDetection {
  claude: CliInfo;
  codex: CliInfo;
  at_ms: number;
}

export interface HistoryStatus {
  enabled: boolean;
  consent: boolean;
  /** 설정한 공급자 */
  provider: "auto" | LlmProvider;
  /** 지금 실제로 쓰게 될 공급자(마지막 감지 기준) — 없으면 모델 없이 찾기만 */
  active: LlmProvider | null;
  detection: LlmDetection | null;
  model_claude: string;
  model_codex: string;
  model: string;
  calls_today: number;
  daily_cap: number;
  /** 이전 버전이 저장한 API 키 파일이 남아 있다(더는 쓰지 않음) */
  legacy_key: boolean;
}

export interface HistoryMsg {
  role: "user" | "assistant";
  content: string;
}

export interface HistorySource {
  n: number;
  session_id: string;
  turn_id: number;
  seq: number;
  at: string;
  session_name: string;
  project: string | null;
  ended: EndedState | null;
  prompt: string;
  cited: boolean;
  tags: TurnTag[];
}

export interface HistoryAnswer {
  text: string;
  sources: HistorySource[];
  model: string | null;
  hits: number;
  ms: number;
  range: [string, string] | null;
}

// ── 예약 전송(0.10.0) ──

export type SchedPolicy = "interrupt" | "after_work" | "after_quiet";
export type SchedMissed = "run_once" | "skip" | "within";
export type SchedWhen = { kind: "after"; min: number } | { kind: "at"; local: string; tz: string };

export interface SchedRun {
  occurrence_at: string;
  /** pending · deferred · fired · delivered · handled · held · missed · failed · cancelled */
  state: string;
  reason: string | null;
  note: string | null;
  reply_id: string | null;
}

export interface SchedItem {
  id: string;
  session_id: string;
  session_name: string;
  text: string;
  quote: QuotePart | null;
  turn_id: number | null;
  created_at: string;
  updated_at: string;
  rev: number;
  kind: "once" | "after";
  tz: string;
  next_due_at: string | null;
  on_missed: SchedMissed;
  missed_within_min: number | null;
  busy_policy: SchedPolicy | null;
  state: "active" | "done" | "cancelled" | "paused";
  run: SchedRun | null;
  atts: AttMeta[];
  warnings?: string[];
  /** 훅이 남긴 세션의 권한 모드(모르면 null) · 예약 시 사전 준비 안내가 필요한가 */
  perm?: string | null;
  sched_warn?: boolean;
}

/** 세션 권한 분류(bypass = 전부 허용) — Rust `sched::perm_kind` 와 같은 값 */
export type PermKind = "bypass" | "auto" | "accept_edits" | "default" | "plan" | "dont_ask" | "unknown";
export interface PermInfo {
  perm: string | null;
  kind: PermKind;
  dismissed: boolean;
  warn: boolean;
  notice: boolean;
}

export interface SchedNew {
  session_id: string;
  text: string;
  atts: string[];
  quote_turn: number | null;
  quote_part: QuotePart | null;
  when: SchedWhen;
  on_missed: SchedMissed;
  missed_within_min: number | null;
  busy_policy: SchedPolicy | null;
}

export interface SchedEdit {
  rev: number;
  text: string;
  atts: string[];
  when: SchedWhen;
  on_missed: SchedMissed;
  missed_within_min: number | null;
  busy_policy: SchedPolicy | null;
}

export interface SchedWindow {
  id: number;
  name: string;
  /** 월(1)~일(64) 비트 */
  days: number;
  start: string;
  end: string;
  tz: string;
  enabled: boolean;
}

export interface SchedSettings {
  paused: boolean;
  busy_default: SchedPolicy;
  perm: "default" | "allowlist";
  allow: { session_id: string; name: string }[];
  windows: SchedWindow[];
  rules: { id: number; scope: "session" | "tag"; key: string; action: SchedPolicy; label: string }[];
  active: number;
  held: number;
}

export const POLICY_LABEL: Record<SchedPolicy, string> = {
  interrupt: "바로 끼움(일하는 중이어도 도구 사이에)",
  after_work: "작업이 끝난 뒤",
  after_quiet: "방해금지 시간이 끝난 뒤",
};

export interface HookStatus {
  settings_path: string;
  installed_events: string[];
  missing_events: string[];
  /** 실행 중인 세션에 말을 넣는 대기 훅이 빠진 이벤트 */
  wake_missing: string[];
  stale_command: string | null;
  /** 훅은 있는데 첨부 이미지 폴더 읽기 허용이 빠짐 */
  read_missing: boolean;
  command: string;
}

/** 화면에 늘 보이는 버전·빌드 시각(정보 창·설정 공용) */
export interface About {
  version: string;
  /** "YYYY-MM-DD HH:mm" — 빌드한 PC 의 시각 */
  build_time: string;
  schema_version: number;
  data_dir: string;
}

export interface AppInfo {
  version: string;
  data_dir: string;
  db_path: string;
  db_bytes: number;
  projects_dir: string;
  sessions: number;
  turns: number;
  installed_at: string | null;
  hook_last_at: string | null;
  backfill_days: number;
  notify: boolean;
  notify_body: boolean;
  notify_min_sec: number;
}

export interface ReplyRow {
  reply_id: string;
  session_id: string | null;
  session_name: string | null;
  card_title: string | null;
  kind: string | null;
  text: string | null;
  received_at: string;
  state: "confirm" | "delivering" | "delivered" | "handled" | "rejected";
  note: string | null;
  device: string | null;
  atts: string[];
}

export interface RelayDevice {
  pid: string;
  name: string;
  can_reply: boolean;
  /** 폰에서 세션 관리(보관함 보기·보관·고정·기록에서 지우기) */
  can_manage: boolean;
  /** 폰에서 예약 전송 만들기·고치기·취소·처리(기본 끔) */
  can_schedule: boolean;
  created_at: string;
  last_seen: string | null;
}

export interface RelayStatus {
  enabled: boolean;
  env: "prod" | "dev";
  host: string;
  status: { state: "off" | "connecting" | "online" | "error" | "upgrade"; error: string | null; since: string | null; phones_online: number };
  devices: RelayDevice[];
  offer: RelayOffer | null;
  pending: { name: string; waited_sec: number } | null;
  push: boolean;
  paused: boolean;
  confirm: boolean;
  bg_resume: boolean;
  pending_confirm: number;
  replies: ReplyRow[];
  mcp_add_command: string;
  start_command: string;
}

export interface RelayOffer {
  uri: string;
  /** Rust 가 만든 QR SVG (외부 라이브러리 없음) */
  svg: string;
  expires_in: number;
}

export interface Available {
  version: string;
  notes: string | null;
  date: string | null;
}

export interface UpdateState {
  current: string;
  /** 업데이트된 뒤 처음 열었으면 그 버전의 바뀐 점(마크다운) */
  whats_new: string | null;
  check: boolean;
  skipped: string | null;
  available: Available | null;
  /** 디스크에 설치된 더 새 버전 — 다시 시작하면 적용 */
  installed: string | null;
  last_check: string | null;
  last_error: string | null;
}

/** 알림 줄의 버튼(되돌리기 등) — 있으면 알림이 조금 더 오래 남는다 */
export interface ToastAction {
  label: string;
  run: () => void | Promise<void>;
}
export type ToastFn = (msg: string, action?: ToastAction) => void;

/** 여러 요청의 읽음이 한꺼번에 바뀌었다(모두 읽음 · 되돌리기) — 열려 있는 대화가 받아 둔 요청에 바로 반영한다.
 *  `at` 이 null 이면 안 읽음으로 되돌린 것 */
export const READ_EVENT = "inbox-read";
export function announceRead(ids: number[], at: string | null) {
  window.dispatchEvent(new CustomEvent(READ_EVENT, { detail: { ids, at } }));
}

/** 한꺼번에 읽음으로 바꾼 요청들과 그 시각 — 되돌리기용 */
export interface ReadBatch {
  ids: number[];
  at: string;
}

/** 세션 안 요청 목차 한 줄 */
export interface OutlineRow {
  id: number;
  seq: number;
  prompt_at: string;
  /** 요청 본문 앞부분(첨부 경로 목록을 뗀 것) */
  head: string;
  slash_command: string | null;
  origin: string | null;
  peer_name: string | null;
  status: Status;
  needs_input: boolean;
  unread: boolean;
  summary: string | null;
  atts: number;
  tags: TurnTag[];
}

export type RelayKey = "enabled" | "env" | "push" | "paused" | "confirm" | "bg_resume";

export type Filter = "all" | "unread" | "attention" | "active" | "history";

/** 폰 답이 세션에 들어갈 때 붙는 머리말 (Rust conoti::REPLY_HEADER 와 같아야 한다) */
export const REPLY_HEADER = "폰에서 온 사용자 답 (AI Inbox · 코노티)";

export type QuotePart = "prompt" | "response";

/** 메신저식 답장 — 보낸 말 본문 맨 앞의 `답장: #12 결과 「발췌」` 한 줄(Rust text::quote_line 과 같아야 한다) */
export interface Quote {
  seq: number;
  part: QuotePart;
  text: string;
}

/** 입력창이 들고 있는 답장 대상 */
export interface QuoteTarget extends Quote {
  turn: number;
}

const QUOTE_RE = /^답장: #(\d+) (요청|결과) 「(.*)」$/;

/** 본문 맨 앞의 답장 줄을 풀어 (답장 대상, 나머지 본문) */
export function splitQuote(body: string): { quote: Quote | null; rest: string } {
  const s = body.replace(/^[\r\n]+/, "");
  const nl = s.indexOf("\n");
  const first = (nl < 0 ? s : s.slice(0, nl)).trimEnd();
  const m = first.match(QUOTE_RE);
  if (!m) return { quote: null, rest: body };
  const rest = nl < 0 ? "" : s.slice(nl + 1).replace(/^[\r\n]+/, "");
  return { quote: { seq: Number(m[1]), part: m[2] === "결과" ? "response" : "prompt", text: m[3] }, rest };
}

/** 답장 미리보기에 쓸 한 줄 — 요청 본문(머리말·앞 답장 줄을 뗀 것) 또는 결과 */
export function quoteExcerpt(t: Turn, part: QuotePart): string {
  const src = part === "response" ? (t.response_text || t.summary || t.understanding || "") : splitQuote(phoneReply(t.prompt_text)?.body ?? t.prompt_text ?? "").rest;
  const flat = src.split("\n").map((l) => l.trim()).filter(Boolean).join(" ").replace(/\*\*/g, "").replace(/`/g, "");
  const max = part === "response" ? 240 : 160;
  return flat.length > max ? `${flat.slice(0, max)}…` : flat;
}

export const quoteLabel = (q: { seq: number; part: QuotePart }) => `#${q.seq} ${q.part === "response" ? "결과" : "요청"}`;

/** 폰에서 온 요청이면 머리말을 떼고 (요청 제목, 답 본문) */
export function phoneReply(prompt: string | null): { title: string | null; body: string } | null {
  if (!prompt || !prompt.startsWith(REPLY_HEADER)) return null;
  const rest = prompt.slice(REPLY_HEADER.length).replace(/^\n/, "");
  const m = rest.match(/^요청: (.*)\n\n([\s\S]*)$/);
  if (m) return { title: m[1], body: m[2] };
  // 이미지만 보낸 답: 경로 목록을 떼면 본문 없이 제목 줄만 남는다
  const only = rest.match(/^요청: ([^\n]*)$/);
  return only ? { title: only[1].trim(), body: "" } : { title: null, body: rest.trim() };
}

export const api = {
  listSessions: (filter: Filter, query: string) => invoke<SessionItem[]>("list_sessions", { filter, query }),
  getChat: (sessionId: string, beforeSeq?: number, limit?: number) =>
    invoke<ChatPage>("get_chat", { sessionId, beforeSeq: beforeSeq ?? null, limit: limit ?? null }),
  getTurn: (turnId: number) => invoke<TurnDetail>("get_turn", { turnId }),
  markRead: (turnIds: number[]) => invoke<void>("mark_read", { turnIds }),
  markUnread: (turnId: number) => invoke<void>("mark_unread", { turnId }),
  /** 세션 하나(없으면 모든 세션)의 안 읽은 결과를 모두 읽음으로 — 되돌리기에 넘길 묶음을 돌려준다 */
  markSessionRead: (sessionId: string | null) => invoke<ReadBatch>("mark_session_read", { sessionId }),
  restoreUnread: (b: ReadBatch) => invoke<number>("restore_unread", { ids: b.ids, at: b.at }),
  sessionOutline: (sessionId: string) => invoke<OutlineRow[]>("session_outline", { sessionId }),
  setStarred: (turnId: number, starred: boolean) => invoke<void>("set_starred", { turnId, starred }),
  setPinned: (sessionId: string, pinned: boolean) => invoke<void>("set_pinned", { sessionId, pinned }),
  setHidden: (sessionId: string, hidden: boolean) => invoke<void>("set_hidden", { sessionId, hidden }),
  counts: () => invoke<Counts>("get_counts"),
  hookStatus: () => invoke<HookStatus>("hook_status"),
  installHooks: () => invoke<HookStatus>("install_hooks"),
  uninstallHooks: () => invoke<HookStatus>("uninstall_hooks"),
  appInfo: () => invoke<AppInfo>("app_info"),
  about: () => invoke<About>("about"),
  setSetting: (key: string, value: number) => invoke<void>("set_setting", { key, value }),
  rescan: () => invoke<void>("rescan"),
  relayStatus: () => invoke<RelayStatus>("relay_status"),
  relaySet: (key: RelayKey, value: string) => invoke<void>("relay_set", { key, value }),
  relayNewOffer: () => invoke<RelayOffer>("relay_new_offer"),
  relayCancelOffer: () => invoke<void>("relay_cancel_offer"),
  /** 보여 준 확인 코드(sas)와 함께 — 그사이 요청이 바뀌었으면 거부된다 */
  relayDecidePair: (approve: boolean, canReply: boolean, sas: string) =>
    invoke<void>("relay_decide_pair", { approve, canReply, sas }),
  relayRemoveDevice: (pid: string) => invoke<void>("relay_remove_device", { pid }),
  relaySetDeviceReply: (pid: string, canReply: boolean) => invoke<void>("relay_set_device_reply", { pid, canReply }),
  relaySetDeviceSchedule: (pid: string, canSchedule: boolean) => invoke<void>("relay_set_device_schedule", { pid, canSchedule }),
  relaySetDeviceManage: (pid: string, canManage: boolean) => invoke<void>("relay_set_device_manage", { pid, canManage }),
  conotiSetSessionMode: (sessionId: string, mode: number) => invoke<void>("conoti_set_session_mode", { sessionId, mode }),
  conotiDecide: (replyId: string, approve: boolean) => invoke<void>("conoti_decide", { replyId, approve }),
  conotiPending: (sessionId: string) => invoke<ReplyRow[]>("conoti_pending", { sessionId }),
  sendMessage: (sessionId: string, text: string, atts: string[] = [], quote?: QuoteTarget | null) =>
    invoke<OutboxItem>("send_message", { sessionId, text, atts, quoteTurn: quote?.turn ?? null, quotePart: quote?.part ?? null }),
  sessionOutbox: (sessionId: string) => invoke<OutboxItem[]>("session_outbox", { sessionId }),
  cancelMessage: (replyId: string) => invoke<void>("cancel_message", { replyId }),
  recentDirs: () => invoke<string[]>("recent_dirs"),
  pickFolder: () => invoke<string | null>("pick_folder"),
  startTask: (cwd: string, text: string, atts: string[] = [], agent: Agent = "claude") =>
    invoke<{ short_id: string; session_id: string | null }>("start_task", { cwd, text, atts, agent }),
  codexStatus: () => invoke<CodexStatus>("codex_status"),
  /** 이미지 원본 바이트를 그대로 넘긴다(base64 로 부풀리지 않게) */
  attachmentPut: (bytes: Uint8Array, name: string) =>
    invoke<AttMeta>("attachment_put", bytes, { headers: { "x-name": encodeURIComponent(name.slice(0, 120)) } }),
  attachmentGet: (id: string, size: AttSize) => invoke<ArrayBuffer>("attachment_get", { id, size }),
  attachmentMeta: (ids: string[]) => invoke<AttMeta[]>("attachment_meta", { ids }),
  attachmentReveal: (id: string) => invoke<void>("attachment_reveal", { id }),
  archiveTurns: (query: string, before: string | null, tags?: TagFilter) =>
    invoke<Page<TurnHit>>("archive_turns", { query, before, limit: 40, tags: tags && filterActive(tags) ? tags : null }),
  archiveMessages: (query: string, device: "all" | "desktop" | "phone", imagesOnly: boolean, before: string | null) =>
    invoke<Page<SentMessage>>("archive_messages", { query, device, imagesOnly, before, limit: 60 }),
  archiveImages: (query: string, before: string | null) => invoke<Page<ArchiveImage>>("archive_images", { query, before, limit: 120 }),
  archiveStats: () => invoke<ArchiveStats>("archive_stats"),
  archiveDeleteMessages: (rids: string[]) => invoke<number>("archive_delete_messages", { rids }),
  archiveDeleteImages: (ids: string[]) => invoke<number>("archive_delete_images", { ids }),
  archiveSessions: (query: string, scope: SessionScope, short: boolean, idleDays: number | null, offset: number) =>
    invoke<Page<SessionRow>>("archive_sessions", { query, scope, short, idleDays, offset }),
  archiveSetArchived: (ids: string[], archived: boolean) => invoke<number>("archive_set_archived", { ids, archived }),
  archiveTidy: (kind: "short" | "idle") => invoke<number>("archive_tidy", { kind }),
  /** 지운 세션 id 와, 진행 중이거나 전달 대기 말이 있어 남긴 세션 이름 */
  archiveDeleteSessions: (ids: string[]) => invoke<{ deleted: string[]; skipped: string[] }>("archive_delete_sessions", { ids }),
  /** /clear 로 끝난 대화의 처리: keep(이력으로 보관) · purge(삭제 예약) · ask(보류) */
  clearDecide: (ids: string[], decision: EndedState) => invoke<number>("clear_decide", { ids, decision }),
  /** onlyUndecided: 아직 안내를 확인하지 않은 것만 · 아니면 삭제 예약·미정 전체 */
  clearList: (onlyUndecided: boolean) => invoke<ClearRow[]>("clear_list", { onlyUndecided }),
  /** 기본 처리를 그대로 두고 안내만 확인 */
  clearAck: (ids: string[]) => invoke<void>("clear_ack", { ids }),
  clearOverview: () => invoke<ClearOverview>("clear_overview"),
  historyStatus: () => invoke<HistoryStatus>("history_status"),
  historySet: (key: "enabled" | "consent" | "provider" | "model_claude" | "model_codex", value: string) => invoke<void>("history_set", { key, value }),
  historyDetect: () => invoke<LlmDetection>("history_detect"),
  /** 빈 문자열이면 지운다. 값은 다시 돌려받지 못한다 */
  historyForgetKey: () => invoke<void>("history_forget_key"),
  schedAdd: (n: SchedNew) => invoke<SchedItem>("sched_add", { new: n }),
  schedList: (sessionId?: string) => invoke<SchedItem[]>("sched_list", { sessionId: sessionId ?? null }),
  schedCounts: () => invoke<{ active: number; held: number }>("sched_counts"),
  schedUpdate: (id: string, edit: SchedEdit) => invoke<SchedItem>("sched_update", { id, edit }),
  schedCancel: (id: string) => invoke<void>("sched_cancel", { id }),
  schedAct: (id: string, op: "send" | "drop") => invoke<void>("sched_act", { id, op }),
  schedSettings: () => invoke<SchedSettings>("sched_settings"),
  schedSetSetting: (key: "paused" | "busy_default" | "perm", value: string) => invoke<void>("sched_set_setting", { key, value }),
  schedPermInfo: (sessionId: string) => invoke<PermInfo>("sched_perm_info", { sessionId }),
  schedWarnOff: (sessionId: string, off: boolean) => invoke<void>("sched_warn_off", { sessionId, off }),
  schedAllowSet: (sessionId: string, on: boolean) => invoke<void>("sched_allow_set", { sessionId, on }),
  schedWindowSave: (w: { id: number | null; name: string; days: number; start: string; end: string; tz: string; enabled: boolean }) => invoke<number>("sched_window_save", w),
  schedWindowDelete: (id: number) => invoke<void>("sched_window_delete", { id }),
  schedRuleSet: (scope: "session" | "tag", key: string, action: SchedPolicy | "") => invoke<void>("sched_rule_set", { scope, key, action }),
  historyAsk: (messages: HistoryMsg[], localOnly = false, tags?: TagFilter) =>
    invoke<HistoryAnswer>("history_ask", { messages, localOnly, tags: tags && filterActive(tags) ? tags : null }),
  // ── 요청 태그 ──
  getChatTagged: (sessionId: string, filter: TagFilter, beforeSeq?: number, limit?: number) =>
    invoke<ChatPage>("get_chat_tagged", { sessionId, filter, beforeSeq: beforeSeq ?? null, limit: limit ?? null }),
  tagOverview: () => invoke<TagOverview>("tag_overview"),
  tagSession: (sessionId: string) => invoke<SessionTags>("tag_session", { sessionId }),
  tagCreate: (name: string, color = "", minor = false) => invoke<number>("tag_create", { name, color, minor }),
  tagUpdate: (id: number, patch: { name?: string; color?: string; minor?: boolean }) =>
    invoke<void>("tag_update", { id, name: patch.name ?? null, color: patch.color ?? null, minor: patch.minor ?? null }),
  tagMerge: (from: number, to: number) => invoke<void>("tag_merge", { from, to }),
  tagDelete: (id: number) => invoke<void>("tag_delete", { id }),
  tagRuleAdd: (tagId: number, kind: "path" | "keyword", pattern: string, suggested = false) =>
    invoke<number>("tag_rule_add", { tagId, kind, pattern, suggested }),
  tagRuleRemove: (id: number) => invoke<void>("tag_rule_remove", { id }),
  tagResetDefaults: () => invoke<number>("tag_reset_defaults"),
  turnTagSet: (turnIds: number[], tagId: number, on: boolean) => invoke<void>("turn_tag_set", { turnIds, tagId, on }),
  tagSuggestFolders: () => invoke<FolderSuggestion[]>("tag_suggest_folders"),
  tagSuggestForTurn: (turnId: number, tagId: number) => invoke<RuleCandidate[]>("tag_suggest_for_turn", { turnId, tagId }),
  tagContext: (sessionId: string, filter: TagFilter, label: string) => invoke<string>("tag_context", { sessionId, filter, label }),
  tagAiStatus: () => invoke<TagAiStatus>("tag_ai_status"),
  tagAiSet: (key: "enabled" | "consent", value: string) => invoke<void>("tag_ai_set", { key, value }),
  tagAiSuggest: () => invoke<{ asked: number; suggested: number }>("tag_ai_suggest"),
  tagAiPending: (limit?: number) => invoke<TagAiPending[]>("tag_ai_pending", { limit: limit ?? null }),
  tagAiDecide: (turnId: number, tagId: number, accept: boolean) => invoke<void>("tag_ai_decide", { turnId, tagId, accept }),
  tagAiDecideAll: (accept: boolean) => invoke<number>("tag_ai_decide_all", { accept }),
  updateState: () => invoke<UpdateState>("update_state"),
  updateAck: () => invoke<void>("update_ack"),
  updateSetCheck: (on: boolean) => invoke<void>("update_set_check", { on }),
  updateSkip: (version: string) => invoke<void>("update_skip", { version }),
  updateCheckNow: () => invoke<Available | null>("update_check_now"),
  updateInstall: () => invoke<void>("update_install"),
  appRestart: () => invoke<void>("app_restart"),
  /** 저장 대화상자를 띄워 고른 곳에만 .md 로 저장. 취소하면 false */
  saveMarkdown: (defaultName: string, content: string) => invoke<boolean>("save_markdown", { defaultName, content }),
  revealDataDir: () => invoke<void>("reveal_data_dir"),
};

export const FINISHED: Status[] = ["done", "interrupted", "stopped"];
export const isFinished = (s: Status) => FINISHED.includes(s);
export const isUnread = (t: Turn) => isFinished(t.status) && !t.read_at;
