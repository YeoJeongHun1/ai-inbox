import { invoke } from "@tauri-apps/api/core";

export type Status = "running" | "background" | "waiting" | "done" | "interrupted" | "stopped";

/** 세션을 만든 도구 */
export type Agent = "claude" | "codex";
export const AGENT_LABEL: Record<Agent, string> = { claude: "Claude Code", codex: "Codex" };

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
  /** 폰 답: 0 받음 · 1 막음 */
  conoti_mode: number;
  channel_live: boolean;
  /** 입력창에서 보내면: live 바로 · queue 일하는 중(끝나면) · approve 권한 승인 대기 · connecting 다시 잇는 중 · resume 꺼진 세션 이어서 실행 · terminal 대기 훅 미설치 */
  send_mode: SendMode;
  /** 실행 중인 백그라운드 세션을 터미널에서 여는 명령 */
  attach_command: string | null;
  agent: Agent;
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
}

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
}

export type RelayKey = "enabled" | "env" | "push" | "paused" | "confirm" | "bg_resume";

export type Filter = "all" | "unread" | "attention" | "active";

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
  archiveTurns: (query: string, before: string | null) => invoke<Page<TurnHit>>("archive_turns", { query, before, limit: 40 }),
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
