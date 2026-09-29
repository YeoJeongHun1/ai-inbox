// 요청 하나를 읽기 좋은 마크다운 문서로. 앱 안에서 보는 것과 .md 로 저장하는 것이 같은 문서다.

import { isFinished, phoneReply, splitQuote, type Step, type TurnDetail } from "./api";
import { clock, duration, fullTime, modelName, num, statusView, tildePath, tokens, toolName, usd } from "./format";

const esc = (s: string) => s.replace(/\|/g, "\\|").replace(/\n/g, " ");

function firstLine(s: string | null | undefined, max = 60): string {
  const line = (s ?? "").split("\n").map((l) => l.trim()).find((l) => l) ?? "";
  return line.length > max ? line.slice(0, max) + "…" : line;
}

/** 답이 필요한 물음: 응답 끝에서 물음표로 끝나는 줄들 */
export function questionsOf(response: string | null | undefined): string[] {
  if (!response) return [];
  const lines = response.split("\n").map((l) => l.trim()).filter(Boolean);
  const tail = lines.slice(-8);
  return tail.filter((l) => /[?？]\s*\**$/.test(l) || /(주세요|알려 주세요|말씀해 주세요)\.?$/.test(l));
}

/** 연속된 도구 호출을 한 줄로 묶는다 */
function renderSteps(steps: Step[]): string[] {
  const out: string[] = [];
  let tools: Step[] = [];
  const flush = () => {
    if (!tools.length) return;
    const counts = new Map<string, number>();
    tools.forEach((t) => counts.set(toolName(t.name), (counts.get(toolName(t.name)) ?? 0) + 1));
    const head = [...counts.entries()].map(([n, c]) => (c > 1 ? `${n} ×${c}` : n)).join(" · ");
    const shown = tools.slice(0, 6).map((t) => `\`${toolName(t.name)}\` ${t.text ?? ""}`.trim());
    out.push(`- ${clock(tools[0].at)} **도구 ${tools.length}회** — ${head}`);
    shown.forEach((s) => out.push(`  - ${s}`));
    if (tools.length > 6) out.push(`  - … 외 ${tools.length - 6}개`);
    tools = [];
  };
  for (const s of steps) {
    if (s.kind === "tool") {
      tools.push(s);
      continue;
    }
    flush();
    const t = (s.text ?? "").trim();
    switch (s.kind) {
      case "text": {
        const body = t.length > 600 ? t.slice(0, 600) + "…" : t;
        out.push(`- ${clock(s.at)} ${body.replace(/\n+/g, " ")}`);
        break;
      }
      case "task":
        out.push(`- ${clock(s.at)} 백그라운드 알림 — ${t}`);
        break;
      case "ask":
        out.push(`- ${clock(s.at)} **${s.name ? `${s.name} 이 보냄` : "작업 중에 받은 말"}** — ${t.replace(/\n+/g, " ")}`);
        break;
      case "summary":
        out.push(`- ${clock(s.at)} 요약 — ${t}`);
        break;
      case "error":
        out.push(`- ${clock(s.at)} 오류 — ${t}`);
        break;
      case "interrupt":
        out.push(`- ${clock(s.at)} 사용자가 중단함`);
        break;
      case "compact":
        out.push(`- ${clock(s.at)} 대화 압축`);
        break;
      case "continue":
        out.push(`- ${clock(s.at)} ${t}`);
        break;
    }
  }
  flush();
  return out;
}

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

  L.push("## 요청");
  L.push("");
  if (t.origin === "peer") L.push(`*다른 세션 \`${t.peer_name ?? "?"}\` 이 보낸 요청*\n`);
  if (t.origin === "inbox") L.push("*AI Inbox 앱에서 보낸 말*\n");
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

  if (t.needs_input) {
    const qs = questionsOf(t.response_text);
    if (qs.length) {
      L.push("## 답이 필요한 질문");
      L.push("");
      qs.forEach((q) => L.push(`- ${q.replace(/^[-*]\s+/, "")}`));
      L.push("");
    }
  }

  L.push("## 작업 요약");
  L.push("");
  if (t.summary) {
    L.push(t.summary);
    L.push("");
  }
  const facts: string[] = [];
  if (d.tools.length) facts.push(`도구 ${num(t.tool_calls)}회 — ${d.tools.map(([n, c]) => `${toolName(n)} ${c}`).join(" · ")}`);
  if (d.files.length) facts.push(`바뀐 파일 ${d.files.length}개`);
  if (d.subagents.length) facts.push(`서브에이전트 ${d.subagents.length}개`);
  if (t.task_notifications) facts.push(`백그라운드 완료 알림 ${t.task_notifications}번`);
  if (t.error_count) facts.push(`오류 ${t.error_count}번`);
  if (!facts.length) facts.push("도구 호출 없이 답함");
  facts.forEach((f) => L.push(`- ${f}`));
  L.push("");

  if (t.understanding && t.understanding !== t.response_text) {
    L.push("### 처음 이해한 내용");
    L.push("");
    L.push(t.understanding.trim());
    L.push("");
  }

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
    L.push("| 종류 | 할 일 | 방식 | 걸린 시간 |");
    L.push("|---|---|---|---|");
    d.subagents.forEach((a) =>
      L.push(
        `| ${esc(a.agent_type ?? "-")} | ${esc(a.description ?? "")} | ${a.background ? "백그라운드" : "대기"} | ${
          a.duration_ms != null ? duration(a.duration_ms) : a.ended_at ? "—" : "진행 중"
        } |`,
      ),
    );
    L.push("");
  }

  L.push("## 응답");
  L.push("");
  L.push(
    t.response_text?.trim() ||
      (t.status === "running"
        ? "_(아직 작업 중)_"
        : t.prompt_source === "mid-turn"
          ? "_(따로 답한 글이 없습니다 — 앞 요청의 작업 과정·응답에 이어집니다)_"
          : "_(응답 없음)_"),
  );
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

  const steps = renderSteps(d.steps.filter((x) => !(x.kind === "text" && x.text?.trim() === t.response_text?.trim())));
  if (steps.length) {
    L.push("## 작업 과정");
    L.push("");
    L.push(...steps);
    L.push("");
  }

  L.push("## 정보");
  L.push("");
  L.push("| 항목 | 값 |");
  L.push("|---|---|");
  const row = (k: string, v: string) => L.push(`| ${k} | ${esc(v)} |`);
  row("상태", st.label);
  row("요청 시각", fullTime(t.prompt_at));
  row("끝난 시각", fullTime(t.ended_at));
  row("걸린 시간", `${duration(t.duration_ms)} (모델 작업 ${duration(t.active_ms)})`);
  row("첫 반응까지", duration(t.ttfr_ms));
  row("모델", `${modelName(t.model)}${t.effort ? ` · effort ${t.effort}` : ""}`);
  row("API 호출", `${num(t.api_calls)}회`);
  row("토큰 — 입력", num(t.input_tokens));
  row("토큰 — 캐시 쓰기 (5분 / 1시간)", `${num(t.cache_create_5m)} / ${num(t.cache_create_1h)}`);
  row("토큰 — 캐시 읽기", num(t.cache_read));
  row("토큰 — 출력 (생각 포함)", `${num(t.output_tokens)}${t.thinking_tokens ? ` (생각 ${num(t.thinking_tokens)})` : ""}`);
  row("끝났을 때 문맥 크기", `${tokens(t.context_tokens)} 토큰`);
  if (t.web_search || t.web_fetch) row("웹 검색 / 가져오기", `${t.web_search} / ${t.web_fetch}`);
  row("작업 폴더", t.cwd ? tildePath(t.cwd) : "—");
  if (t.git_branch && t.git_branch !== "HEAD") row("브랜치", t.git_branch);
  row("세션", `${s.name} (${s.id})`);
  if (s.cost_usd != null) row("세션 누적 비용 (API 환산)", usd(s.cost_usd));
  L.push("");

  if (d.hooks.length) {
    L.push("### 훅 이벤트");
    L.push("");
    d.hooks.forEach((h) => {
      const msg = (h.detail?.message as string) || (h.detail?.source as string) || (h.detail?.reason as string) || "";
      L.push(`- ${clock(h.at)} \`${h.event}\`${msg ? ` — ${msg}` : ""}`);
    });
    L.push("");
  }

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
