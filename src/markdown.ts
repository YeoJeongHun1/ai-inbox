// 요청 하나를 읽기 좋은 마크다운 문서로. 앱 안에서 보는 것과 .md 로 저장하는 것이 같은 문서다.

import { isFinished, phoneReply, splitQuote, type TurnDetail } from "./api";
import { clock, duration, fullTime, modelName, statusView, tildePath, tokens, toolName } from "./format";
import { attentionOf, costParts, processItems, processSummary, SECTION, subagentLine, understandingOf, type ProcessItem } from "./turncard";

const esc = (s: string) => s.replace(/\|/g, "\\|").replace(/\n/g, " ");

function firstLine(s: string | null | undefined, max = 60): string {
  const line = (s ?? "").split("\n").map((l) => l.trim()).find((l) => l) ?? "";
  return line.length > max ? line.slice(0, max) + "…" : line;
}

/** 과정 칸의 시간순 목록 — 같은 도구가 이어지면 한 줄로 묶는다 */
function renderProcess(items: ProcessItem[]): string[] {
  const out: string[] = [];
  for (const it of items) {
    const at = clock(it.at);
    if (it.kind === "tools") {
      const name = toolName(it.name);
      if (it.count === 1) {
        out.push(`- ${at} \`${name}\` ${(it.texts[0] ?? "").replace(/\n+/g, " ")}`.trimEnd());
        continue;
      }
      out.push(`- ${at} \`${name}\` ×${it.count}`);
      it.texts.slice(0, 6).forEach((x) => out.push(`  - ${x.replace(/\n+/g, " ")}`));
      if (it.texts.length > 6) out.push(`  - … 외 ${it.texts.length - 6}개`);
      continue;
    }
    const t = it.text;
    switch (it.kind) {
      case "text": {
        const body = t.length > 600 ? t.slice(0, 600) + "…" : t;
        out.push(`- ${at} ${body.replace(/\n+/g, " ")}`);
        break;
      }
      case "task":
        out.push(`- ${at} 백그라운드 알림 — ${t}`);
        break;
      case "ask":
        out.push(`- ${at} **${it.name ? `${it.name} 이 보냄` : "작업 중에 받은 말"}** — ${t.replace(/\n+/g, " ")}`);
        break;
      case "summary":
        out.push(`- ${at} 요약 — ${t}`);
        break;
      case "error":
        out.push(`- ${at} **오류** — ${t}`);
        break;
      case "interrupt":
        out.push(`- ${at} **사용자가 중단함**`);
        break;
      case "compact":
        out.push(`- ${at} 대화 압축`);
        break;
      case "continue":
        out.push(`- ${at} ${t}`);
        break;
    }
  }
  return out;
}

/**
 * 턴 카드 문서 — 질문 · 이해 · 결과 · 응답 필요 · 과정 · 비용 순서(src/turncard.ts).
 * 🚨 src-tauri/src/doc.rs(폰 문서)가 같은 구성이다 — 바꾸면 둘을 같이.
 */
export function buildTurnMarkdown(d: TurnDetail): string {
  const t = d.turn;
  const s = d.session;
  const st = statusView(t.status, t.needs_input, t.pending_bg);
  const L: string[] = [];

  const title = firstLine(phoneReply(t.prompt_text)?.body ?? t.prompt_text, 70) || t.slash_command || `요청 #${t.seq}`;
  L.push(`# ${title}`);
  L.push("");
  const branch = s.git_branch && s.git_branch !== "HEAD" ? s.git_branch : null;
  const where = [s.name, s.project_name, branch].filter(Boolean).join(" · ");
  L.push(`> ${where}  `);
  L.push(`> ${fullTime(t.prompt_at)} · **${st.label}** · ${duration(t.duration_ms)}`);
  L.push("");

  // 1. 질문
  L.push(`## ${SECTION.prompt}`);
  L.push("");
  if (t.origin === "peer") L.push(`*다른 세션 \`${t.peer_name ?? "?"}\` 이 보낸 요청*\n`);
  if (t.origin === "inbox") L.push("*AI Inbox 앱에서 보낸 말*\n");
  if (t.origin === "sched") L.push("*AI Inbox 예약 전송으로 보낸 말*\n");
  const phone = phoneReply(t.prompt_text);
  if (phone) L.push(`*폰(코노티)에서 보낸 답${phone.title ? ` — 원래 요청: ${phone.title}` : ""}*\n`);
  if (t.prompt_source === "mid-turn") L.push("*앞 요청이 진행되는 중에 보낸 말*\n");
  if (t.slash_command) L.push(`\`${t.slash_command}\`\n`);
  const atts = t.atts?.length ?? 0;
  // 답장이면 인용으로(Rust doc.rs 와 같게)
  const { quote, rest } = splitQuote((phone ? phone.body : t.prompt_text) ?? "");
  if (quote) L.push(`> **#${quote.seq} ${quote.part === "response" ? "결과" : "요청"}에 답장** — ${quote.text}\n`);
  L.push(rest.trim() || (atts ? "_(이미지만 보냄)_" : "_(본문 없음)_"));
  L.push("");
  if (atts) L.push(`*첨부 이미지 ${atts}장*\n`);

  // 2. 이해 — 착수 멘트·응답 되풀이는 뺀다
  const understanding = understandingOf(t.understanding, t.response_text);
  if (understanding) {
    L.push(`## ${SECTION.understanding}`);
    L.push("");
    L.push(understanding);
    L.push("");
  }

  // 3. 결과
  L.push(`## ${SECTION.result}`);
  L.push("");
  if (t.summary) {
    L.push(`**${t.summary.trim()}**`);
    L.push("");
  }
  L.push(
    t.response_text?.trim() ||
      (t.status === "running"
        ? "_(아직 작업 중)_"
        : t.prompt_source === "mid-turn"
          ? "_(따로 답한 글이 없습니다 — 앞 요청의 작업 과정·응답에 이어집니다)_"
          : "_(응답 없음)_"),
  );
  L.push("");

  // 4. 응답 필요 — 있을 때만. 확정(질문 도구·권한 승인)과 추정(글 끝 물음)을 가른다
  const att = attentionOf(t, d.steps);
  if (att) {
    L.push(`## ${SECTION.attention}`);
    L.push("");
    L.push(att.sure ? `**${att.label}**` : `**${att.label}** _(추정 — 응답 끝의 물음으로 짐작)_`);
    L.push("");
    att.items.forEach((q) => L.push(`- ${q.replace(/\n+/g, " ")}`));
    if (att.items.length) L.push("");
  }

  // 5. 과정
  L.push(`## ${SECTION.process}`);
  L.push("");
  L.push(
    processSummary(
      { toolCalls: t.tool_calls, tools: d.tools, files: d.files.length, subagents: d.subagents.length, errors: t.error_count },
      toolName,
    ) + (t.status === "interrupted" ? " · 중단됨" : ""),
  );
  L.push("");
  const plan = parsePlan(t.plan_json);
  if (plan.length) {
    L.push("### 계획");
    L.push("");
    plan.forEach((p) => L.push(`- [${p.status === "completed" ? "x" : " "}] ${p.text}`));
    L.push("");
  }
  if (d.files.length) {
    L.push("### 바뀐 파일");
    L.push("");
    d.files.forEach(([p, n]) => L.push(`- \`${shortPath(p, t.cwd)}\`${n > 1 ? ` (${n}번 수정)` : ""}`));
    L.push("");
  }
  if (d.subagents.length) {
    L.push("### 서브에이전트");
    L.push("");
    d.subagents.forEach((a) => L.push(`- ${subagentLine(a, duration)}`));
    L.push("");
  }
  const items = renderProcess(processItems(d.steps, t.response_text, understanding));
  if (items.length) {
    L.push("### 시간순");
    L.push("");
    L.push(...items);
    L.push("");
  }

  // 6. 비용 — 달러는 넣지 않는다
  L.push(`## ${SECTION.cost}`);
  L.push("");
  const cost = costParts(t, { model: modelName, tokens, duration });
  L.push(cost.length ? cost.join(" · ") : "—");
  L.push("");
  L.push("| 항목 | 값 |");
  L.push("|---|---|");
  const row = (k: string, v: string) => L.push(`| ${k} | ${esc(v)} |`);
  row("끝난 시각", fullTime(t.ended_at));
  row("작업 폴더", t.cwd ? tildePath(t.cwd) : "—");
  if (t.git_branch && t.git_branch !== "HEAD") row("브랜치", t.git_branch);
  row("세션", `${s.name} (${s.id})`);
  L.push("");

  L.push("## 이어진 요청");
  L.push("");
  if (d.next_id) {
    const body = (d.next_prompt ?? "").trim();
    const lines = body.split("\n").filter((l) => l.trim());
    L.push(`*${fullTime(d.next_at)} · 같은 세션의 다음 요청*`);
    L.push("");
    lines.slice(0, 8).forEach((l) => L.push(`> ${l}`));
    if (lines.length > 8) L.push(`> …`);
    if (!lines.length) L.push("> _(본문 없음)_");
  } else {
    L.push(isFinished(t.status) ? "_아직 이어진 요청이 없습니다._" : "_작업이 끝나면 다음 요청이 여기에 이어집니다._");
  }
  L.push("");

  return L.join("\n");
}

export interface PlanItem {
  text: string;
  status: string;
}

export function parsePlan(json: string | null): PlanItem[] {
  if (!json) return [];
  try {
    return JSON.parse(json) as PlanItem[];
  } catch {
    return [];
  }
}

export function shortPath(p: string, cwd: string | null): string {
  if (cwd && p.startsWith(cwd)) {
    const rest = p.slice(cwd.length).replace(/^[/\\]+/, "");
    if (rest) return rest;
  }
  return tildePath(p);
}

export function suggestedFileName(d: TurnDetail): string {
  const t = d.turn;
  const date = fullTime(t.prompt_at).slice(0, 10);
  const head = firstLine(t.prompt_text, 40)
    .replace(/[\\/:*?"<>|#\n]/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  return `${date} ${d.session.name} ${head || "요청"}.md`.replace(/\s+/g, " ");
}
