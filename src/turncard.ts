// 턴 카드 — 요청 하나를 6칸(질문 · 이해 · 결과 · 응답 필요 · 과정 · 비용) 고정 순서로 보여 주는 규칙.
// 화면(TurnCard·AiBubble)과 문서(markdown.ts)가 같이 쓴다. 폰 문서(src-tauri/src/doc.rs)에 같은 규칙의 Rust 판이 있다 —
// 🚨 규칙을 바꾸면 둘을 같이 바꾸고, 공용 시험값 `src-tauri/tests/fixtures/turn_card_vectors.json` 에 사례를 더한다.
// 이 파일은 런타임 import 가 없어야 한다(`node --test tests/` 가 타입만 지우고 그대로 읽는다).

import type { Step, SubRow, Turn } from "./api";

export const SECTION = {
  prompt: "질문",
  understanding: "이해",
  result: "결과",
  attention: "응답 필요",
  process: "과정",
  cost: "비용",
} as const;

const trimEnd = (s: string) => s.replace(/[\s.!…:~*_`)]+$/u, "");

/** 문장 나누기 — 줄바꿈과 마침표·물음표·느낌표 뒤 공백 */
export function sentences(s: string): string[] {
  return s
    .split(/\n+|(?<=[.!?。？！])\s+/u)
    .map((x) => x.trim())
    .filter(Boolean);
}

// 상태·결과를 말하는 끝맺음 — 착수 멘트가 아니다(입니다·있습니다·했습니다 …)
const STATE_END =
  /(입니다|있습니다|없습니다|됩니다|같습니다|보입니다|맞습니다|많습니다|적습니다|다릅니다|[했었았였됐]습니다|필요합니다|가능합니다|중요합니다|야 합니다)$/u;
// 착수 멘트 — "~하겠습니다 / 확인해 보겠습니다 / 확인합니다 / 살펴봅니다 / 할게요 / Let me …"
const ONSET_END = /(겠습니다|겠어요|[가-힣]게요|[가-힣]니다)$/u;
const ONSET_EN = /^(i'll|i will|let me|let's|i'm going to|first,? i'll|now i'll)\b/i;

/**
 * 화면·문서에 보일 "이해 요약". 없거나 보일 값어치가 없으면 null.
 * - 응답과 같거나 응답의 첫머리를 그대로 되풀이하면 null (중복 노출 방지)
 * - 1~2문장이고 마지막 문장이 착수 멘트(하겠습니다·확인합니다·Let me …)면 null
 */
export function understandingOf(understanding: string | null | undefined, response?: string | null): string | null {
  const u = (understanding ?? "").trim();
  if (!u) return null;
  const r = (response ?? "").trim();
  if (r && (u === r || r.startsWith(u))) return null;
  const ss = sentences(u);
  if (ss.length <= 2) {
    const last = trimEnd(ss[ss.length - 1] ?? "");
    // "겠습니다" 는 STATE_END(했습니다 …)에 걸리지 않으니 상태 검사를 먼저 해도 된다
    if (ONSET_EN.test(last) || (ONSET_END.test(last) && !STATE_END.test(last))) return null;
  }
  return u;
}

/** 한 줄로 — 화면의 이해 칸은 첫 줄만(전체는 title 로) */
export function oneLine(s: string, max = 200): string {
  const line = s.replace(/\s*\n+\s*/g, " ").trim();
  return line.length > max ? line.slice(0, max) + "…" : line;
}

/** 결과 첫 단락. 코드 블록 안의 빈 줄은 나누지 않고, 제목만 있는 단락은 다음 단락까지 붙인다. */
export function firstParagraph(md: string | null | undefined): { head: string; more: boolean } {
  const text = (md ?? "").trim();
  if (!text) return { head: "", more: false };
  const lines = text.split("\n");
  const blocks: string[][] = [];
  let cur: string[] = [];
  let fence = false;
  for (const l of lines) {
    if (/^\s*(```|~~~)/.test(l)) fence = !fence;
    if (!fence && !l.trim()) {
      if (cur.length) blocks.push(cur);
      cur = [];
      continue;
    }
    cur.push(l);
  }
  if (cur.length) blocks.push(cur);
  let take = 0;
  const head: string[] = [];
  while (take < blocks.length) {
    const b = blocks[take++];
    head.push(b.join("\n"));
    const onlyHeading = b.every((l) => /^\s*#{1,6}\s/.test(l) || /^\s*(---|\*\*\*)\s*$/.test(l));
    if (!onlyHeading) break;
  }
  return { head: head.join("\n\n"), more: take < blocks.length };
}

/** 답이 필요한 물음(추정): 응답 끝 8줄 중 물음표·"~주세요"로 끝나는 줄 */
export function questionsOf(response: string | null | undefined): string[] {
  if (!response) return [];
  const lines = response.split("\n").map((l) => l.trim()).filter(Boolean);
  return lines
    .slice(-8)
    .filter((l) => /[?？]\s*\**$/.test(l) || /(주세요|알려 주세요|말씀해 주세요)\.?$/.test(l))
    .map((l) => l.replace(/^[-*]\s+/, ""));
}

export interface Attention {
  /** 확정 신호(질문 도구 대기·권한 승인 대기)인가, 글 끝을 보고 짐작한 것인가 */
  sure: boolean;
  label: string;
  /** 물음 목록(질문 도구의 물음 또는 응답 끝의 물음) */
  items: string[];
}

type TurnLike = Pick<Turn, "status" | "needs_input" | "response_text"> & { last_step?: Turn["last_step"] };

/** 마지막 도구 단계 [이름, 글] — 상세가 있으면 단계 목록에서, 없으면 목록용 last_step 에서 */
function lastTool(t: TurnLike, steps?: Step[]): [string | null, string | null] | null {
  if (steps) {
    for (let i = steps.length - 1; i >= 0; i--) {
      if (steps[i].kind === "tool") return [steps[i].name, steps[i].text];
    }
    return null;
  }
  const s = t.last_step;
  return s && s[0] === "tool" ? [s[1], s[2]] : null;
}

/**
 * 사용자 응답 필요 칸. 없으면 null.
 * - status=waiting: 확정. 마지막 도구가 AskUserQuestion 이면 "질문에 답 필요", 상세 단계에서 다른 도구면 "권한 승인 대기",
 *   목록만으로 가릴 수 없으면 "승인·답변 대기".
 * - 끝났는데 needs_input(응답 끝이 물음): 추정.
 * 작업 중에 받은 말(step kind "ask")은 다른 쪽이 보낸 말이라 여기 넣지 않는다.
 */
export function attentionOf(t: TurnLike, steps?: Step[]): Attention | null {
  if (t.status === "waiting") {
    const tool = lastTool(t, steps);
    if (tool && tool[0] === "AskUserQuestion") return { sure: true, label: "질문에 답 필요", items: tool[1] ? [tool[1]] : [] };
    if (steps && tool) return { sure: true, label: "권한 승인 대기", items: [] };
    return { sure: true, label: "승인·답변 대기", items: [] };
  }
  if (t.status === "done" && t.needs_input) {
    return { sure: false, label: "답이 필요해 보임", items: questionsOf(t.response_text) };
  }
  return null;
}

/** 많이 쓴 도구 상위 k 종 */
export function topTools(tools: [string, number][], k = 3): [string, number][] {
  return [...tools].sort((a, b) => b[1] - a[1]).slice(0, k);
}

export interface ProcessCounts {
  toolCalls: number;
  /** [이름, 횟수] — 상세가 있을 때만(목록에서는 모른다) */
  tools?: [string, number][];
  files: number;
  subagents: number;
  errors: number;
}

/** 과정 요약 한 줄 — "도구 12회(Bash 5, Read 4, Edit 2) · 파일 3 · 서브에이전트 1 · 오류 1" */
export function processSummary(c: ProcessCounts, name: (n: string) => string = (n) => n): string {
  const parts: string[] = [];
  if (c.toolCalls > 0) {
    const top = c.tools?.length ? topTools(c.tools).map(([n, k]) => `${name(n)} ${k}`).join(", ") : "";
    parts.push(`도구 ${c.toolCalls}회${top ? `(${top})` : ""}`);
  }
  if (c.files > 0) parts.push(`파일 ${c.files}`);
  if (c.subagents > 0) parts.push(`서브에이전트 ${c.subagents}`);
  if (c.errors > 0) parts.push(`오류 ${c.errors}`);
  return parts.length ? parts.join(" · ") : "도구 없이 답함";
}

export type ProcessItem =
  | { kind: "tools"; at: string | null; name: string | null; count: number; texts: string[] }
  | { kind: Exclude<Step["kind"], "tool">; at: string | null; name: string | null; text: string };

/**
 * 과정 칸의 시간순 목록. 같은 도구가 이어지면 한 묶음, 응답·이해 칸과 같은 글 단계는 뺀다(중복 노출 방지).
 */
export function processItems(steps: Step[], response?: string | null, understanding?: string | null): ProcessItem[] {
  const skip = new Set([(response ?? "").trim(), (understanding ?? "").trim()].filter(Boolean));
  const out: ProcessItem[] = [];
  for (const s of steps) {
    if (s.kind === "tool") {
      const last = out[out.length - 1];
      if (last && last.kind === "tools" && last.name === s.name) {
        last.count++;
        if (s.text) last.texts.push(s.text);
      } else {
        out.push({ kind: "tools", at: s.at, name: s.name, count: 1, texts: s.text ? [s.text] : [] });
      }
      continue;
    }
    const text = (s.text ?? "").trim();
    if (s.kind === "text" && skip.has(text)) continue;
    out.push({ kind: s.kind, at: s.at, name: s.name, text });
  }
  return out;
}

/** 서브에이전트 한 줄 — "general-purpose · 설명 · 3분 12초 · 백그라운드" */
export function subagentLine(a: SubRow, dur: (ms: number) => string): string {
  const took = a.duration_ms != null ? dur(a.duration_ms) : a.ended_at ? "—" : "진행 중";
  return [a.agent_type ?? "-", (a.description ?? "").replace(/\s*\n+\s*/g, " ").trim() || "-", took, a.background ? "백그라운드" : ""]
    .filter(Boolean)
    .join(" · ");
}

type CostTurn = Pick<
  Turn,
  "model" | "effort" | "input_tokens" | "output_tokens" | "cache_read" | "cache_create_5m" | "cache_create_1h" | "active_ms" | "tool_calls"
>;

/**
 * 비용 칸 한 줄. 캐시는 입력과 합치지 않고 따로(합치면 입력이 수십 배로 부푼다). 달러는 넣지 않는다.
 * 각 조각은 [이름, 값] — 화면은 조각마다 칸을, 문서는 " · " 로 잇는다.
 */
export function costParts(
  t: CostTurn,
  f: { model: (m: string | null) => string; tokens: (n: number) => string; duration: (ms: number) => string },
): string[] {
  const out: string[] = [];
  if (t.model) out.push(`${f.model(t.model)}${t.effort ? ` · effort ${t.effort}` : ""}`);
  if (t.input_tokens || t.output_tokens) out.push(`입력 ${f.tokens(t.input_tokens)} / 출력 ${f.tokens(t.output_tokens)}`);
  const write = (t.cache_create_5m ?? 0) + (t.cache_create_1h ?? 0);
  if (t.cache_read || write) out.push(`캐시 읽기 ${f.tokens(t.cache_read)} · 쓰기 ${f.tokens(write)}`);
  if (t.active_ms != null && t.active_ms > 0) out.push(`작업 ${f.duration(t.active_ms)}`);
  if (t.tool_calls > 0) out.push(`도구 ${t.tool_calls}회`);
  return out;
}
