import type { Ended, Status } from "./api";

const WEEK = ["일", "월", "화", "수", "목", "금", "토"];

const d = (iso: string) => new Date(iso);
const sameDay = (a: Date, b: Date) =>
  a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
const pad = (n: number) => String(n).padStart(2, "0");

export function clock(iso: string | null | undefined): string {
  if (!iso) return "";
  const t = d(iso);
  return `${pad(t.getHours())}:${pad(t.getMinutes())}`;
}

/** 목록 오른쪽 시각: 오늘은 시:분, 어제, 이번 주는 요일, 그 전은 월/일 */
export function listTime(iso: string | null | undefined): string {
  if (!iso) return "";
  const t = d(iso);
  const now = new Date();
  if (sameDay(t, now)) return clock(iso);
  const y = new Date(now);
  y.setDate(now.getDate() - 1);
  if (sameDay(t, y)) return "어제";
  const diffDays = (now.getTime() - t.getTime()) / 86_400_000;
  if (diffDays < 6) return WEEK[t.getDay()];
  return `${t.getMonth() + 1}/${t.getDate()}`;
}

export function dayKey(iso: string): string {
  const t = d(iso);
  return `${t.getFullYear()}-${t.getMonth()}-${t.getDate()}`;
}

export function dayParts(iso: string): { day: string; rest: string } {
  const t = d(iso);
  const now = new Date();
  const rest = sameDay(t, now) ? "오늘" : `${t.getMonth() + 1}월 ${WEEK[t.getDay()]}요일`;
  return { day: String(t.getDate()), rest };
}

export function fullTime(iso: string | null | undefined): string {
  if (!iso) return "—";
  const t = d(iso);
  return `${t.getFullYear()}-${pad(t.getMonth() + 1)}-${pad(t.getDate())} ${pad(t.getHours())}:${pad(t.getMinutes())}:${pad(t.getSeconds())}`;
}

/** 3분 12초 · 1시간 4분 · 42초 */
export function duration(ms: number | null | undefined): string {
  if (ms == null) return "—";
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}초`;
  const m = Math.floor(s / 60);
  if (m < 60) return s % 60 ? `${m}분 ${s % 60}초` : `${m}분`;
  const h = Math.floor(m / 60);
  return m % 60 ? `${h}시간 ${m % 60}분` : `${h}시간`;
}

/** 말풍선 모서리의 큰 숫자: 0:42 · 3:12 · 1:04:10 */
export function numeral(ms: number | null | undefined): string {
  if (ms == null) return "";
  const s = Math.max(0, Math.round(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  return h ? `${h}:${pad(m)}:${pad(sec)}` : `${m}:${pad(sec)}`;
}

export function tokens(n: number | null | undefined): string {
  if (!n) return "0";
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
  return `${(n / 1_000_000).toFixed(n < 10_000_000 ? 2 : 1)}M`;
}

export function num(n: number | null | undefined): string {
  return (n ?? 0).toLocaleString("ko-KR");
}

export function usd(n: number | null | undefined): string {
  if (n == null) return "—";
  return `$${n.toFixed(n < 10 ? 2 : 1)}`;
}

/** claude-opus-5-5[1m] → Opus 5.5 · 1M · gpt-6-astra → GPT-6 Astra (src-tauri/src/doc.rs 의 model_name 과 같은 규칙) */
export function modelName(m: string | null | undefined): string {
  if (!m) return "—";
  const gpt = /^gpt-([^-]+)(?:-(.+))?$/i.exec(m);
  if (gpt) {
    const rest = (gpt[2] ?? "").split("-").filter(Boolean).map((w) => w[0].toUpperCase() + w.slice(1));
    return `GPT-${gpt[1]}${rest.length ? " " + rest.join(" ") : ""}`;
  }
  const oneM = /\[1m\]/i.test(m);
  const base = m.replace(/\[1m\]/i, "").replace(/^claude-/, "").replace(/-\d{8}$/, "");
  const parts = base.split("-");
  const family = parts[0] ? parts[0][0].toUpperCase() + parts[0].slice(1) : base;
  const ver = parts.slice(1).join(".");
  return `${family}${ver ? " " + ver : ""}${oneM ? " · 1M" : ""}`;
}

export interface StatusView {
  label: string;
  tone: "done" | "live" | "attention" | "muted";
}

export function statusView(status: Status, needsInput: boolean, pendingBg = 0): StatusView {
  switch (status) {
    case "running":
      return { label: "작업 중", tone: "live" };
    case "background":
      return { label: pendingBg > 1 ? `백그라운드 작업 ${pendingBg}개 대기` : "백그라운드 작업 대기", tone: "live" };
    case "waiting":
      return { label: "승인·답변 대기", tone: "attention" };
    case "interrupted":
      return { label: "중단됨", tone: "muted" };
    case "stopped":
      return { label: "멈춤", tone: "muted" };
    default:
      return needsInput ? { label: "답이 필요해요", tone: "attention" } : { label: "완료", tone: "done" };
  }
}

export function sinceNow(iso: string | null | undefined): number | null {
  if (!iso) return null;
  return Date.now() - d(iso).getTime();
}

/** 마크다운 원문에서 첫 문단 한두 줄 — 목록·알림용 */
export function plainLine(md: string | null | undefined, max = 140): string {
  if (!md) return "";
  const line = md
    .split("\n")
    .map((l) => l.replace(/^#+\s*|^[-*>]\s+|\*\*|`/g, "").trim())
    .find((l) => l.length > 0);
  if (!line) return "";
  return line.length > max ? line.slice(0, max) + "…" : line;
}

/** mcp__computer-use__screenshot → computer-use:screenshot */
export function toolName(n: string | null | undefined): string {
  if (!n) return "도구";
  const m = n.match(/^mcp__(.+?)__(.+)$/);
  return m ? `${m[1]}:${m[2]}` : n;
}

/** 홈 폴더를 ~ 로 */
export function tildePath(p: string): string {
  return p.replace(/^\/Users\/[^/]+/, "~").replace(/^[A-Za-z]:\\Users\\[^\\]+/, "~");
}

/** 삭제 예정까지 남은 일수(올림, 0 이상). 예정 시각이 없으면 null */
export function daysLeft(iso: string | null | undefined): number | null {
  if (!iso) return null;
  return Math.max(0, Math.ceil((new Date(iso).getTime() - Date.now()) / 86_400_000));
}

/** /clear 로 끝난 대화의 한 줄 표시: "끝남 · 삭제 예정 12일 후" · "끝남 · 이력 보관" · "끝남 · 처리 미정" */
export function endedLabel(e: Ended): string {
  if (e.state === "keep") return "끝남 · 이력 보관";
  if (e.state === "ask") return "끝남 · 처리 미정";
  const n = daysLeft(e.purge_at);
  return n == null ? "끝남 · 삭제 예약" : n === 0 ? "끝남 · 곧 삭제" : `끝남 · 삭제 예정 ${n}일 후`;
}
